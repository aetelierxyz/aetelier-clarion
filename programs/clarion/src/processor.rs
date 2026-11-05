use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use solana_system_interface::{
    instruction as system_instruction, program as system_program,
};

use crate::{
    error::ClarionError,
    event::ClarionEvent,
    instruction::ClarionInstruction,
    state::{
        ARM_COUNT, Arm, ArmAggregates, AuthorityProposal, MAX_GENESIS_LEAD_SLOTS,
        PROPOSAL_DISCRIMINATOR, PROPOSAL_SEED, PROPOSAL_SIZE, STATE_DISCRIMINATOR,
        STATE_SEED, STATE_SIZE, State, WINDOW_DISCRIMINATOR, WINDOW_SEED, WINDOW_SIZE,
        Window, proposal_address, state_address, window_address, window_bounds,
    },
};

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match ClarionInstruction::unpack(instruction_data)? {
        ClarionInstruction::Init {
            genesis_slot,
            window_len,
            min_reveal_lag_slots,
        } => init(
            program_id,
            accounts,
            genesis_slot,
            window_len,
            min_reveal_lag_slots,
        ),
        ClarionInstruction::Commit {
            window_id,
            slot_start,
            slot_end,
            merkle_root,
        } => commit(
            program_id,
            accounts,
            window_id,
            slot_start,
            slot_end,
            merkle_root,
        ),
        ClarionInstruction::Reveal {
            window_id,
            aggregates,
        } => reveal(program_id, accounts, window_id, aggregates),
        ClarionInstruction::ProposeAuthority { new_authority } => {
            propose_authority(program_id, accounts, new_authority)
        }
        ClarionInstruction::AcceptAuthority => accept_authority(program_id, accounts),
        ClarionInstruction::CancelAuthorityProposal => {
            cancel_authority_proposal(program_id, accounts)
        }
    }
}

fn init(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    genesis_slot: u64,
    window_len: u64,
    min_reveal_lag_slots: u64,
) -> ProgramResult {
    let [
        authority,
        state_account,
        system_program_account,
        program_account,
    ] = accounts
    else {
        return Err(ClarionError::AccountCountMismatch.into());
    };
    require_signer(authority)?;
    require_system_program(system_program_account)?;
    require_program_signature(program_id, program_account)?;
    if window_len == 0 {
        return Err(ClarionError::WindowLenZero.into());
    }
    if min_reveal_lag_slots == 0 {
        return Err(ClarionError::MinRevealLagZero.into());
    }
    if min_reveal_lag_slots > window_len {
        return Err(ClarionError::RevealLagExceedsWindowLen.into());
    }
    let genesis_lead = genesis_slot
        .checked_sub(Clock::get()?.slot)
        .ok_or(ClarionError::GenesisSlotPassed)?;
    if genesis_lead > MAX_GENESIS_LEAD_SLOTS {
        return Err(ClarionError::GenesisSlotTooFar.into());
    }
    if window_bounds(genesis_slot, window_len, 0).is_none() {
        return Err(ClarionError::WindowBoundsOverflow.into());
    }
    let (state_key, bump) = state_address(program_id);
    if *state_account.key != state_key {
        return Err(ClarionError::StateAddressMismatch.into());
    }
    if !is_absent(state_account)? {
        return Err(ClarionError::StateAlreadyInitialized.into());
    }

    create_program_account(
        authority,
        state_account,
        system_program_account,
        program_id,
        STATE_SIZE,
        &[STATE_SEED, &[bump]],
    )?;
    let state = State {
        discriminator: STATE_DISCRIMINATOR,
        authority: *authority.key,
        genesis_slot,
        window_len,
        min_reveal_lag_slots,
        next_window_id: 0,
        bump,
    };
    state.pack(&mut state_account.try_borrow_mut_data()?)?;
    ClarionEvent::Initialized {
        authority: state.authority,
        genesis_slot,
        window_len,
        min_reveal_lag_slots,
    }
    .emit()
}

fn commit(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    window_id: u64,
    slot_start: u64,
    slot_end: u64,
    merkle_root: [u8; 32],
) -> ProgramResult {
    let (authority, state_account, window_account, system_program_account, previous) =
        match (accounts, window_id.checked_sub(1)) {
            (
                [
                    authority,
                    state_account,
                    window_account,
                    system_program_account,
                ],
                None,
            ) => (
                authority,
                state_account,
                window_account,
                system_program_account,
                None,
            ),
            (
                [
                    authority,
                    state_account,
                    window_account,
                    system_program_account,
                    previous_window_account,
                ],
                Some(previous_window_id),
            ) => (
                authority,
                state_account,
                window_account,
                system_program_account,
                Some((previous_window_account, previous_window_id)),
            ),
            _ => return Err(ClarionError::AccountCountMismatch.into()),
        };
    require_signer(authority)?;
    require_system_program(system_program_account)?;
    let mut state = load_state(program_id, state_account)?;
    if state.authority != *authority.key {
        return Err(ClarionError::AuthorityMismatch.into());
    }
    if window_id != state.next_window_id {
        return Err(ClarionError::WindowIdNotSequential.into());
    }
    if let Some((previous_window_account, previous_window_id)) = previous {
        let previous_window =
            load_window(program_id, previous_window_account, previous_window_id)?;
        if !previous_window.is_revealed() {
            return Err(ClarionError::PreviousWindowNotRevealed.into());
        }
    }
    let (grid_start, grid_end) =
        window_bounds(state.genesis_slot, state.window_len, window_id)
            .ok_or(ClarionError::WindowBoundsOverflow)?;
    if slot_start != grid_start {
        return Err(ClarionError::SlotStartOffGrid.into());
    }
    if slot_end != grid_end {
        return Err(ClarionError::SlotEndOffGrid.into());
    }
    let commit_slot = Clock::get()?.slot;
    if slot_end >= commit_slot {
        return Err(ClarionError::WindowNotClosed.into());
    }
    let (window_key, bump) = window_address(program_id, window_id);
    if *window_account.key != window_key {
        return Err(ClarionError::WindowAddressMismatch.into());
    }
    if !is_absent(window_account)? {
        return Err(ClarionError::WindowAlreadyCommitted.into());
    }
    state.next_window_id = window_id
        .checked_add(1)
        .ok_or(ClarionError::WindowBoundsOverflow)?;

    create_program_account(
        authority,
        window_account,
        system_program_account,
        program_id,
        WINDOW_SIZE,
        &[WINDOW_SEED, &window_id.to_le_bytes(), &[bump]],
    )?;
    let window = Window {
        discriminator: WINDOW_DISCRIMINATOR,
        window_id,
        slot_start,
        slot_end,
        merkle_root,
        commit_slot,
        reveal_slot: 0,
        aggregates: Arm::ALL.map(ArmAggregates::empty),
        bump,
    };
    window.pack(&mut window_account.try_borrow_mut_data()?)?;
    state.pack(&mut state_account.try_borrow_mut_data()?)?;
    ClarionEvent::Committed {
        window_id,
        slot_start,
        slot_end,
        merkle_root,
        commit_slot,
    }
    .emit()
}

fn reveal(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    window_id: u64,
    aggregates: [ArmAggregates; ARM_COUNT],
) -> ProgramResult {
    let [authority, state_account, window_account] = accounts else {
        return Err(ClarionError::AccountCountMismatch.into());
    };
    require_signer(authority)?;
    let state = load_state(program_id, state_account)?;
    if state.authority != *authority.key {
        return Err(ClarionError::AuthorityMismatch.into());
    }
    let mut window = load_window(program_id, window_account, window_id)?;
    if window.is_revealed() {
        return Err(ClarionError::WindowAlreadyRevealed.into());
    }
    let reveal_slot = Clock::get()?.slot;
    let lag_elapsed = reveal_slot
        .checked_sub(window.commit_slot)
        .is_some_and(|lag| lag >= state.min_reveal_lag_slots);
    if !lag_elapsed {
        return Err(ClarionError::RevealLagNotElapsed.into());
    }
    if aggregates.map(|entry| entry.arm) != Arm::ALL {
        return Err(ClarionError::ArmOutOfOrder.into());
    }
    if aggregates
        .iter()
        .any(|arm| arm.tx_landed > arm.tx_submitted)
    {
        return Err(ClarionError::LandedExceedsSubmitted.into());
    }

    window.aggregates = aggregates;
    window.reveal_slot = reveal_slot;
    window.pack(&mut window_account.try_borrow_mut_data()?)?;
    ClarionEvent::Revealed {
        window_id,
        reveal_slot,
        aggregates,
    }
    .emit()
}

fn propose_authority(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    new_authority: Pubkey,
) -> ProgramResult {
    let [
        authority,
        state_account,
        proposal_account,
        system_program_account,
    ] = accounts
    else {
        return Err(ClarionError::AccountCountMismatch.into());
    };
    require_signer(authority)?;
    require_system_program(system_program_account)?;
    let state = load_state(program_id, state_account)?;
    if state.authority != *authority.key {
        return Err(ClarionError::AuthorityMismatch.into());
    }
    if new_authority == state.authority {
        return Err(ClarionError::AuthorityUnchanged.into());
    }
    let (proposal_key, bump) = proposal_address(program_id);
    if *proposal_account.key != proposal_key {
        return Err(ClarionError::ProposalAddressMismatch.into());
    }
    let proposal = if is_absent(proposal_account)? {
        create_program_account(
            authority,
            proposal_account,
            system_program_account,
            program_id,
            PROPOSAL_SIZE,
            &[PROPOSAL_SEED, &[bump]],
        )?;
        AuthorityProposal {
            discriminator: PROPOSAL_DISCRIMINATOR,
            proposed_authority: new_authority,
            bump,
        }
    } else {
        AuthorityProposal {
            proposed_authority: new_authority,
            ..load_proposal(program_id, proposal_account)?
        }
    };
    proposal.pack(&mut proposal_account.try_borrow_mut_data()?)?;
    ClarionEvent::AuthorityProposed {
        authority: state.authority,
        proposed_authority: proposal.proposed_authority,
    }
    .emit()
}

fn accept_authority(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [new_authority, state_account, proposal_account] = accounts else {
        return Err(ClarionError::AccountCountMismatch.into());
    };
    require_signer(new_authority)?;
    let mut state = load_state(program_id, state_account)?;
    let proposal = load_present_proposal(program_id, proposal_account)?;
    if proposal.proposed_authority != *new_authority.key {
        return Err(ClarionError::ProposedAuthorityMismatch.into());
    }

    let previous_authority = state.authority;
    state.authority = proposal.proposed_authority;
    state.pack(&mut state_account.try_borrow_mut_data()?)?;
    close_program_account(proposal_account, new_authority)?;
    ClarionEvent::AuthorityAccepted {
        previous_authority,
        authority: state.authority,
    }
    .emit()
}

fn cancel_authority_proposal(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let [authority, state_account, proposal_account] = accounts else {
        return Err(ClarionError::AccountCountMismatch.into());
    };
    require_signer(authority)?;
    let state = load_state(program_id, state_account)?;
    if state.authority != *authority.key {
        return Err(ClarionError::AuthorityMismatch.into());
    }
    let proposal = load_present_proposal(program_id, proposal_account)?;

    close_program_account(proposal_account, authority)?;
    ClarionEvent::AuthorityProposalCancelled {
        authority: state.authority,
        proposed_authority: proposal.proposed_authority,
    }
    .emit()
}

fn require_signer(authority: &AccountInfo) -> Result<(), ClarionError> {
    if authority.is_signer {
        Ok(())
    } else {
        Err(ClarionError::AuthoritySignatureMissing)
    }
}

fn require_program_signature(
    program_id: &Pubkey,
    program_account: &AccountInfo,
) -> Result<(), ClarionError> {
    if program_account.is_signer && program_account.key == program_id {
        Ok(())
    } else {
        Err(ClarionError::ProgramSignatureMissing)
    }
}

fn require_system_program(account: &AccountInfo) -> Result<(), ClarionError> {
    if system_program::check_id(account.key) {
        Ok(())
    } else {
        Err(ClarionError::SystemProgramMismatch)
    }
}

fn is_absent(account: &AccountInfo) -> Result<bool, ProgramError> {
    Ok(system_program::check_id(account.owner) && account.try_data_is_empty()?)
}

fn load_state(program_id: &Pubkey, account: &AccountInfo) -> Result<State, ProgramError> {
    if account.owner != program_id {
        return Err(ClarionError::StateOwnerMismatch.into());
    }
    let state = State::unpack(&account.try_borrow_data()?)?;
    let derived =
        Pubkey::create_program_address(&[STATE_SEED, &[state.bump]], program_id);
    if derived.ok().as_ref() != Some(account.key) {
        return Err(ClarionError::StateAddressMismatch.into());
    }
    Ok(state)
}

fn load_window(
    program_id: &Pubkey,
    account: &AccountInfo,
    window_id: u64,
) -> Result<Window, ProgramError> {
    if account.owner != program_id {
        return Err(ClarionError::WindowOwnerMismatch.into());
    }
    let window = Window::unpack(&account.try_borrow_data()?)?;
    let derived = Pubkey::create_program_address(
        &[WINDOW_SEED, &window_id.to_le_bytes(), &[window.bump]],
        program_id,
    );
    if derived.ok().as_ref() != Some(account.key) {
        return Err(ClarionError::WindowAddressMismatch.into());
    }
    Ok(window)
}

fn load_proposal(
    program_id: &Pubkey,
    account: &AccountInfo,
) -> Result<AuthorityProposal, ProgramError> {
    if account.owner != program_id {
        return Err(ClarionError::ProposalOwnerMismatch.into());
    }
    let proposal = AuthorityProposal::unpack(&account.try_borrow_data()?)?;
    let derived =
        Pubkey::create_program_address(&[PROPOSAL_SEED, &[proposal.bump]], program_id);
    if derived.ok().as_ref() != Some(account.key) {
        return Err(ClarionError::ProposalAddressMismatch.into());
    }
    Ok(proposal)
}

fn load_present_proposal(
    program_id: &Pubkey,
    account: &AccountInfo,
) -> Result<AuthorityProposal, ProgramError> {
    if *account.key != proposal_address(program_id).0 {
        return Err(ClarionError::ProposalAddressMismatch.into());
    }
    if is_absent(account)? {
        return Err(ClarionError::ProposalMissing.into());
    }
    load_proposal(program_id, account)
}

fn close_program_account(target: &AccountInfo, recipient: &AccountInfo) -> ProgramResult {
    let mut target_lamports = target.try_borrow_mut_lamports()?;
    let mut recipient_lamports = recipient.try_borrow_mut_lamports()?;
    **recipient_lamports = recipient_lamports
        .checked_add(**target_lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **target_lamports = 0;
    target.resize(0)?;
    target.assign(&system_program::ID);
    Ok(())
}

fn create_program_account<'a>(
    payer: &AccountInfo<'a>,
    target: &AccountInfo<'a>,
    system_program_account: &AccountInfo<'a>,
    program_id: &Pubkey,
    size: usize,
    signer_seeds: &[&[u8]],
) -> ProgramResult {
    let rent_exempt_minimum = Rent::get()?.minimum_balance(size);
    let shortfall = rent_exempt_minimum
        .checked_sub(target.try_lamports()?)
        .filter(|lamports| *lamports > 0);
    if let Some(shortfall) = shortfall {
        invoke(
            &system_instruction::transfer(payer.key, target.key, shortfall),
            &[
                payer.clone(),
                target.clone(),
                system_program_account.clone(),
            ],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(target.key, size as u64),
        &[target.clone(), system_program_account.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(target.key, program_id),
        &[target.clone(), system_program_account.clone()],
        &[signer_seeds],
    )
}
