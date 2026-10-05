use clarion_pulsar::{
    output::CsvFile,
    rpc::RpcClient,
    validators::{Snapshot, ValidatorRow, join},
};

use crate::support::{CannedRpc, RpcReply, STAMP, fixture_text, scratch_dir};

const SAMPLED: u64 = 1_790_000_000_123_456;

fn fixtures(method: &str) -> RpcReply {
    let name = match method {
        "getClusterNodes" => "get_cluster_nodes.json",
        "getVoteAccounts" => "get_vote_accounts.json",
        "getBlockProduction" => "get_block_production.json",
        "getLeaderSchedule" => "get_leader_schedule.json",
        _ => "get_epoch_info.json",
    };
    RpcReply::ok(fixture_text(name))
}

async fn fixture_rows() -> Vec<ValidatorRow> {
    let rpc = CannedRpc::by_method(fixtures).await;
    let client = RpcClient::new(rpc.url.as_str()).unwrap();
    let info = client.epoch_info().await.unwrap();
    let nodes = client.cluster_nodes().await.unwrap();
    let accounts = client.vote_accounts().await.unwrap();
    let production = client.block_production().await.unwrap();
    let schedule = client
        .leader_schedule(info.first_slot().unwrap())
        .await
        .unwrap()
        .unwrap();
    join(&Snapshot {
        sampled_ts_us: SAMPLED,
        cluster: "devnet",
        epoch: info.epoch,
        nodes: &nodes.kept,
        vote_accounts: &accounts.kept,
        block_production: &production.kept,
        leader_slots: &schedule.kept,
    })
}

#[tokio::test]
async fn fixtures_join_into_one_row_per_identity_and_vote_account_in_stake_order() {
    let rows = fixture_rows().await;

    let directory = scratch_dir("validators-join");
    let mut file = CsvFile::create_validators(&directory, STAMP).unwrap();
    file.append(&rows).unwrap();
    let text = std::fs::read_to_string(file.path()).unwrap();

    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "sampled_ts_us,cluster,epoch,identity,vote_account,gossip,tpu_quic,version,\
             activated_stake_lamports,commission,delinquent,last_vote,root_slot,epoch_credits,\
             leader_slots,blocks_produced,scheduled_leader_slots",
            "1790000000123456,devnet,7,J69vQ4CoH3AA221Q8BGxAyPq2rXeDhqug5gSrVMG3wKJ,\
             DYwdq2BMNEHdyTiwTCJFhAQfoFf3tVNGFWmBYkGrMDvP,,,,7000000000000,0,false,1001,969,0,\
             4,4,8",
            "1790000000123456,devnet,7,F2K3Fm5Vm26tz7ENRmu1RetmC2K91Lciz1bVZG3b3J2V,\
             BrwXyt6bUR4mTkY7jMX2eebFLUZNoiAFZXFQtncL3VD5,192.0.2.10:8001,192.0.2.10:8009,\
             3.1.13,5000000000000,5,false,1000,968,150,8,7,4",
            "1790000000123456,devnet,7,6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC,\
             F3k12Ti63TcudEdvr9cNkaigbi7NoVqKwmVJSKrDo1D6,192.0.2.20:8001,198.51.100.7:11228,\
             3.1.13,2000000000,100,false,998,966,20,0,0,0",
            "1790000000123456,devnet,7,HAzUQNXVVQ6qrAXBBTVHkkFouc8rBJz5DGiCKFrDtSa9,\
             BMQNgtjCLYmcJjJzCrRHU1QxBHQTp9QCCquPVUwSiRuU,,203.0.113.50:8009,3.1.13,\
             2000000000,7,true,700,668,15,0,0,0",
            "1790000000123456,devnet,7,6WK8ze98CueYEekV3SopfDeUUeKcoB7AjVknciENUBZC,\
             6qeRCKXpJZtYfDb7CpFcxFTY1kBQWLuSEaeEy1rbpX3j,192.0.2.20:8001,198.51.100.7:11228,\
             3.1.13,0,10,true,500,468,0,0,0,0",
            "1790000000123456,devnet,7,3PCWRNyvSZnSi47hM9w88uF96jwiBAM9CoU5y6RA39em,,\
             192.0.2.40:8001,,1.18.26,,,,,,,0,0,0",
            "1790000000123456,devnet,7,3QpLzoyAR7Yav7Mmbmve64jFQCMcy9wjrL75W4cvXwAf,,,\
             [2001:db8::60]:8009,,,,,,,,0,0,0",
            "1790000000123456,devnet,7,D1qNfH759x48mNvFoacjBzbso16tpyftzJGrFkvc52Uj,,\
             192.0.2.30:8001,,,,,,,,,0,0,0",
        ]
    );
}

#[tokio::test]
async fn every_gossip_node_and_every_valid_vote_account_appears() {
    let rows = fixture_rows().await;

    let with_stake = rows
        .iter()
        .filter(|row| row.activated_stake_lamports.is_some())
        .count();
    let gossip_only = rows.iter().filter(|row| row.vote_account.is_none()).count();
    let outside_gossip = rows
        .iter()
        .filter(|row| {
            row.vote_account.is_some() && row.gossip.is_none() && row.tpu_quic.is_none()
        })
        .count();

    assert_eq!(rows.len(), 8);
    assert_eq!(with_stake, 5);
    assert_eq!(gossip_only, 3);
    assert_eq!(outside_gossip, 1);
}
