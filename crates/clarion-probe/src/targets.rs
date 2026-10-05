use std::net::{IpAddr, SocketAddr};

use crate::rpc::ClusterNode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub pubkey: String,
    pub tpu_quic: SocketAddr,
    pub gossip_ip: Option<IpAddr>,
}

pub fn select_targets(nodes: &[ClusterNode]) -> Vec<Target> {
    nodes.iter().filter_map(target_of).collect()
}

fn target_of(node: &ClusterNode) -> Option<Target> {
    let tpu_quic = node.tpu_quic.as_deref()?.parse().ok()?;
    let gossip_ip = node
        .gossip
        .as_deref()
        .and_then(|address| address.parse::<SocketAddr>().ok())
        .map(|address| address.ip());
    Some(Target {
        pubkey: node.pubkey.clone(),
        tpu_quic,
        gossip_ip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(pubkey: &str, gossip: Option<&str>, tpu_quic: Option<&str>) -> ClusterNode {
        ClusterNode {
            pubkey: pubkey.to_owned(),
            gossip: gossip.map(str::to_owned),
            tpu_quic: tpu_quic.map(str::to_owned),
        }
    }

    #[test]
    fn entries_without_tpu_quic_are_dropped() {
        let nodes = [
            node("A", Some("192.0.2.1:8001"), Some("192.0.2.1:8009")),
            node("B", Some("192.0.2.2:8001"), None),
            node("C", None, None),
        ];

        let targets = select_targets(&nodes);

        assert_eq!(
            targets,
            vec![Target {
                pubkey: "A".to_owned(),
                tpu_quic: "192.0.2.1:8009".parse().unwrap(),
                gossip_ip: Some("192.0.2.1".parse().unwrap()),
            }]
        );
    }

    #[test]
    fn entry_with_unparsable_tpu_quic_is_dropped() {
        let nodes = [node("A", Some("192.0.2.1:8001"), Some("not-an-address"))];

        assert!(select_targets(&nodes).is_empty());
    }

    #[test]
    fn absent_gossip_leaves_gossip_ip_empty() {
        let nodes = [node("A", None, Some("192.0.2.1:8009"))];

        let targets = select_targets(&nodes);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].gossip_ip, None);
    }

    #[test]
    fn gossip_ip_is_kept_when_it_differs_from_the_tpu_quic_host() {
        let nodes = [node("A", Some("192.0.2.1:8001"), Some("198.51.100.7:8009"))];

        let targets = select_targets(&nodes);

        assert_eq!(
            targets[0].tpu_quic.ip(),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(targets[0].gossip_ip, Some("192.0.2.1".parse().unwrap()));
    }

    #[test]
    fn selection_preserves_cluster_order() {
        let nodes = [
            node("A", None, Some("192.0.2.1:8009")),
            node("B", None, None),
            node("C", None, Some("[2001:db8::3]:8009")),
        ];

        let pubkeys: Vec<String> = select_targets(&nodes)
            .into_iter()
            .map(|target| target.pubkey)
            .collect();

        assert_eq!(pubkeys, ["A", "C"]);
    }
}
