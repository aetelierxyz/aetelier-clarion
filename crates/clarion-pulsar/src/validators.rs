use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    net::SocketAddr,
};

use serde::{Deserialize, Serialize};

use crate::rpc::{BlockProduction, ClusterNode, LeaderSlots, VoteAccount, VoteAccounts};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorRow {
    pub sampled_ts_us: u64,
    pub cluster: String,
    pub epoch: u64,
    pub identity: String,
    pub vote_account: Option<String>,
    pub gossip: Option<SocketAddr>,
    pub tpu_quic: Option<SocketAddr>,
    pub version: Option<String>,
    pub activated_stake_lamports: Option<u64>,
    pub commission: Option<u8>,
    pub delinquent: Option<bool>,
    pub last_vote: Option<u64>,
    pub root_slot: Option<u64>,
    pub epoch_credits: Option<u64>,
    pub leader_slots: u64,
    pub blocks_produced: u64,
    pub scheduled_leader_slots: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot<'a> {
    pub sampled_ts_us: u64,
    pub cluster: &'a str,
    pub epoch: u64,
    pub nodes: &'a [ClusterNode],
    pub vote_accounts: &'a VoteAccounts,
    pub block_production: &'a BlockProduction,
    pub leader_slots: &'a LeaderSlots,
}

#[derive(Default)]
struct Identity<'a> {
    node: Option<&'a ClusterNode>,
    votes: Vec<(&'a VoteAccount, bool)>,
}

pub fn join(snapshot: &Snapshot<'_>) -> Vec<ValidatorRow> {
    let mut identities: BTreeMap<&str, Identity<'_>> = BTreeMap::new();
    for node in snapshot.nodes {
        let entry = identities.entry(node.pubkey.as_str()).or_default();
        entry.node.get_or_insert(node);
    }
    let mut seen_votes = HashSet::new();
    let tagged = snapshot
        .vote_accounts
        .current
        .iter()
        .map(|account| (account, false))
        .chain(
            snapshot
                .vote_accounts
                .delinquent
                .iter()
                .map(|account| (account, true)),
        );
    for (account, delinquent) in tagged {
        if seen_votes.insert((account.node_pubkey.as_str(), account.vote_pubkey.as_str()))
        {
            identities
                .entry(account.node_pubkey.as_str())
                .or_default()
                .votes
                .push((account, delinquent));
        }
    }
    let mut rows: Vec<ValidatorRow> = identities
        .into_iter()
        .flat_map(|(identity, entry)| identity_rows(snapshot, identity, entry))
        .collect();
    rows.sort_by(row_order);
    rows
}

fn identity_rows(
    snapshot: &Snapshot<'_>,
    identity: &str,
    entry: Identity<'_>,
) -> Vec<ValidatorRow> {
    let production = snapshot
        .block_production
        .get(identity)
        .copied()
        .unwrap_or_default();
    let base = ValidatorRow {
        sampled_ts_us: snapshot.sampled_ts_us,
        cluster: snapshot.cluster.to_owned(),
        epoch: snapshot.epoch,
        identity: identity.to_owned(),
        vote_account: None,
        gossip: entry.node.and_then(|node| socket(node.gossip.as_deref())),
        tpu_quic: entry.node.and_then(|node| socket(node.tpu_quic.as_deref())),
        version: entry.node.and_then(|node| node.version.clone()),
        activated_stake_lamports: None,
        commission: None,
        delinquent: None,
        last_vote: None,
        root_slot: None,
        epoch_credits: None,
        leader_slots: production.leader_slots,
        blocks_produced: production.blocks_produced,
        scheduled_leader_slots: snapshot.leader_slots.get(identity).copied().unwrap_or(0),
    };
    if entry.votes.is_empty() {
        return vec![base];
    }
    entry
        .votes
        .into_iter()
        .map(|(account, delinquent)| ValidatorRow {
            vote_account: Some(account.vote_pubkey.clone()),
            activated_stake_lamports: Some(account.activated_stake),
            commission: Some(account.commission),
            delinquent: Some(delinquent),
            last_vote: Some(account.last_vote),
            root_slot: Some(account.root_slot),
            epoch_credits: Some(
                account.epoch_credits.map_or(0, |credits| credits.earned()),
            ),
            ..base.clone()
        })
        .collect()
}

fn socket(address: Option<&str>) -> Option<SocketAddr> {
    address?.parse().ok()
}

fn row_order(left: &ValidatorRow, right: &ValidatorRow) -> Ordering {
    stake_order(
        left.activated_stake_lamports,
        right.activated_stake_lamports,
    )
    .then_with(|| left.identity.cmp(&right.identity))
    .then_with(|| left.vote_account.cmp(&right.vote_account))
}

fn stake_order(left: Option<u64>, right: Option<u64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{EpochCredits, Production};

    fn node(pubkey: &str, gossip: &str, tpu_quic: Option<&str>) -> ClusterNode {
        ClusterNode {
            pubkey: pubkey.to_owned(),
            gossip: Some(gossip.to_owned()),
            tpu_quic: tpu_quic.map(str::to_owned),
            version: Some("3.1.13".to_owned()),
        }
    }

    fn vote(vote_pubkey: &str, node_pubkey: &str, stake: u64) -> VoteAccount {
        VoteAccount {
            vote_pubkey: vote_pubkey.to_owned(),
            node_pubkey: node_pubkey.to_owned(),
            activated_stake: stake,
            commission: 5,
            last_vote: 1_000,
            root_slot: 968,
            epoch_credits: Some(EpochCredits {
                epoch: 7,
                credits: 600,
                previous_credits: 450,
            }),
        }
    }

    fn rows(
        nodes: &[ClusterNode],
        vote_accounts: &VoteAccounts,
        block_production: &BlockProduction,
        leader_slots: &LeaderSlots,
    ) -> Vec<ValidatorRow> {
        join(&Snapshot {
            sampled_ts_us: 1_790_000_000_123_456,
            cluster: "devnet",
            epoch: 7,
            nodes,
            vote_accounts,
            block_production,
            leader_slots,
        })
    }

    fn keys(rows: &[ValidatorRow]) -> Vec<(&str, Option<&str>)> {
        rows.iter()
            .map(|row| (row.identity.as_str(), row.vote_account.as_deref()))
            .collect()
    }

    #[test]
    fn gossip_node_without_a_vote_account_gets_one_row_with_empty_vote_fields() {
        let nodes = [node("A", "192.0.2.1:8001", Some("192.0.2.1:8009"))];

        let rows = rows(
            &nodes,
            &VoteAccounts::default(),
            &BlockProduction::new(),
            &LeaderSlots::new(),
        );

        assert_eq!(
            rows,
            vec![ValidatorRow {
                sampled_ts_us: 1_790_000_000_123_456,
                cluster: "devnet".to_owned(),
                epoch: 7,
                identity: "A".to_owned(),
                vote_account: None,
                gossip: Some("192.0.2.1:8001".parse().unwrap()),
                tpu_quic: Some("192.0.2.1:8009".parse().unwrap()),
                version: Some("3.1.13".to_owned()),
                activated_stake_lamports: None,
                commission: None,
                delinquent: None,
                last_vote: None,
                root_slot: None,
                epoch_credits: None,
                leader_slots: 0,
                blocks_produced: 0,
                scheduled_leader_slots: 0,
            }]
        );
    }

    #[test]
    fn vote_account_absent_from_gossip_gets_one_row_with_empty_network_fields() {
        let accounts = VoteAccounts {
            current: vec![vote("V", "B", 9)],
            delinquent: Vec::new(),
        };

        let rows = rows(&[], &accounts, &BlockProduction::new(), &LeaderSlots::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].identity, "B");
        assert_eq!(rows[0].vote_account.as_deref(), Some("V"));
        assert_eq!(rows[0].gossip, None);
        assert_eq!(rows[0].tpu_quic, None);
        assert_eq!(rows[0].version, None);
        assert_eq!(rows[0].activated_stake_lamports, Some(9));
        assert_eq!(rows[0].delinquent, Some(false));
        assert_eq!(rows[0].epoch_credits, Some(150));
    }

    #[test]
    fn identity_with_several_vote_accounts_gets_one_row_per_account() {
        let nodes = [node("A", "192.0.2.1:8001", None)];
        let accounts = VoteAccounts {
            current: vec![vote("V2", "A", 5)],
            delinquent: vec![vote("V1", "A", 5)],
        };

        let rows = rows(
            &nodes,
            &accounts,
            &BlockProduction::new(),
            &LeaderSlots::new(),
        );

        assert_eq!(keys(&rows), [("A", Some("V1")), ("A", Some("V2"))]);
        assert_eq!(rows[0].delinquent, Some(true));
        assert_eq!(rows[1].delinquent, Some(false));
        assert!(
            rows.iter()
                .all(|row| row.gossip == Some("192.0.2.1:8001".parse().unwrap()))
        );
    }

    #[test]
    fn rows_are_ordered_by_stake_descending_then_identity_then_vote_account() {
        let nodes = [
            node("Z", "192.0.2.9:8001", None),
            node("A", "192.0.2.1:8001", None),
            node("M", "192.0.2.5:8001", None),
        ];
        let accounts = VoteAccounts {
            current: vec![
                vote("V9", "M", 10),
                vote("V3", "C", 30),
                vote("V2", "B", 10),
                vote("V1", "B", 10),
            ],
            delinquent: vec![vote("V0", "D", 0)],
        };

        let rows = rows(
            &nodes,
            &accounts,
            &BlockProduction::new(),
            &LeaderSlots::new(),
        );

        assert_eq!(
            keys(&rows),
            [
                ("C", Some("V3")),
                ("B", Some("V1")),
                ("B", Some("V2")),
                ("M", Some("V9")),
                ("D", Some("V0")),
                ("A", None),
                ("Z", None),
            ]
        );
    }

    #[test]
    fn production_and_schedule_counts_attach_by_identity_and_default_to_zero() {
        let nodes = [
            node("A", "192.0.2.1:8001", None),
            node("B", "192.0.2.2:8001", None),
        ];
        let accounts = VoteAccounts {
            current: vec![vote("V1", "A", 2), vote("V2", "A", 1)],
            delinquent: Vec::new(),
        };
        let production = BlockProduction::from([(
            "A".to_owned(),
            Production {
                leader_slots: 8,
                blocks_produced: 7,
            },
        )]);
        let schedule = LeaderSlots::from([("A".to_owned(), 12), ("B".to_owned(), 4)]);

        let rows = rows(&nodes, &accounts, &production, &schedule);

        let counts: Vec<(&str, u64, u64, u64)> = rows
            .iter()
            .map(|row| {
                (
                    row.identity.as_str(),
                    row.leader_slots,
                    row.blocks_produced,
                    row.scheduled_leader_slots,
                )
            })
            .collect();
        assert_eq!(counts, [("A", 8, 7, 12), ("A", 8, 7, 12), ("B", 0, 0, 4)]);
    }

    #[test]
    fn epoch_credits_are_zero_when_the_history_is_empty_or_regresses() {
        let mut empty = vote("V1", "A", 1);
        empty.epoch_credits = None;
        let mut regressed = vote("V2", "B", 1);
        regressed.epoch_credits = Some(EpochCredits {
            epoch: 7,
            credits: 10,
            previous_credits: 20,
        });
        let accounts = VoteAccounts {
            current: vec![empty, regressed],
            delinquent: Vec::new(),
        };

        let rows = rows(&[], &accounts, &BlockProduction::new(), &LeaderSlots::new());

        assert_eq!(
            rows.iter().map(|row| row.epoch_credits).collect::<Vec<_>>(),
            [Some(0), Some(0)]
        );
    }

    #[test]
    fn unparsable_network_addresses_are_left_empty() {
        let nodes = [node("A", "not-an-address", Some("192.0.2.1:99999"))];

        let rows = rows(
            &nodes,
            &VoteAccounts::default(),
            &BlockProduction::new(),
            &LeaderSlots::new(),
        );

        assert_eq!(rows[0].gossip, None);
        assert_eq!(rows[0].tpu_quic, None);
    }

    #[test]
    fn repeated_entries_keep_their_first_occurrence() {
        let nodes = [
            node("A", "192.0.2.1:8001", None),
            node("A", "192.0.2.2:8001", None),
        ];
        let accounts = VoteAccounts {
            current: vec![vote("V1", "A", 3)],
            delinquent: vec![vote("V1", "A", 0)],
        };

        let rows = rows(
            &nodes,
            &accounts,
            &BlockProduction::new(),
            &LeaderSlots::new(),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].gossip, Some("192.0.2.1:8001".parse().unwrap()));
        assert_eq!(rows[0].activated_stake_lamports, Some(3));
        assert_eq!(rows[0].delinquent, Some(false));
    }
}
