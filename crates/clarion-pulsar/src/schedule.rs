use std::{
    collections::{HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Start(SocketAddr),
    WaitUntil(Instant),
    WaitForCompletion,
    Finished,
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    pending: VecDeque<SocketAddr>,
    busy_ips: HashSet<IpAddr>,
    in_flight: usize,
    concurrency: usize,
    start_interval: Duration,
    next_start: Option<Instant>,
}

pub fn start_interval(max_starts_per_second: u16) -> Duration {
    Duration::from_secs(1)
        .checked_div(u32::from(max_starts_per_second.max(1)))
        .unwrap_or(Duration::from_secs(1))
}

pub fn round_order(addresses: &[SocketAddr], rng: &mut fastrand::Rng) -> Vec<SocketAddr> {
    let mut order = addresses.to_vec();
    rng.shuffle(&mut order);
    order
}

impl Scheduler {
    pub fn new(
        order: Vec<SocketAddr>,
        concurrency: u16,
        start_interval: Duration,
    ) -> Self {
        Self {
            pending: order.into(),
            busy_ips: HashSet::new(),
            in_flight: 0,
            concurrency: usize::from(concurrency.max(1)),
            start_interval,
            next_start: None,
        }
    }

    pub fn next(&mut self, now: Instant) -> Next {
        if self.pending.is_empty() {
            return if self.in_flight == 0 {
                Next::Finished
            } else {
                Next::WaitForCompletion
            };
        }
        if self.in_flight >= self.concurrency {
            return Next::WaitForCompletion;
        }
        let Some(position) = self
            .pending
            .iter()
            .position(|address| !self.busy_ips.contains(&address.ip()))
        else {
            return Next::WaitForCompletion;
        };
        if let Some(at) = self.next_start
            && now < at
        {
            return Next::WaitUntil(at);
        }
        let Some(address) = self.pending.remove(position) else {
            return Next::WaitForCompletion;
        };
        self.busy_ips.insert(address.ip());
        self.in_flight = self.in_flight.saturating_add(1);
        self.next_start = now.checked_add(self.start_interval);
        Next::Start(address)
    }

    pub fn complete(&mut self, address: SocketAddr) {
        if self.busy_ips.remove(&address.ip()) {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEN_MS: Duration = Duration::from_millis(10);

    fn addresses(list: &[&str]) -> Vec<SocketAddr> {
        list.iter()
            .map(|address| address.parse().unwrap())
            .collect()
    }

    fn started(next: Next) -> SocketAddr {
        match next {
            Next::Start(address) => address,
            other => panic!("expected a start, got {other:?}"),
        }
    }

    #[test]
    fn one_hundred_starts_per_second_space_starts_ten_milliseconds_apart() {
        assert_eq!(start_interval(100), TEN_MS);
        assert_eq!(start_interval(1), Duration::from_secs(1));
        assert_eq!(start_interval(0), Duration::from_secs(1));
    }

    #[test]
    fn second_start_waits_for_the_start_budget() {
        let order = addresses(&["192.0.2.1:8009", "192.0.2.2:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 64, TEN_MS);
        let now = Instant::now();

        assert_eq!(scheduler.next(now), Next::Start(order[0]));
        assert_eq!(scheduler.next(now), Next::WaitUntil(now + TEN_MS));
        assert_eq!(
            scheduler.next(now + TEN_MS - Duration::from_nanos(1)),
            Next::WaitUntil(now + TEN_MS)
        );
        assert_eq!(scheduler.next(now + TEN_MS), Next::Start(order[1]));
    }

    #[test]
    fn starts_never_exceed_the_rate_over_a_simulated_second() {
        let order: Vec<SocketAddr> = (0..=255_u8)
            .map(|host| SocketAddr::from(([198, 51, 100, host], 8009)))
            .collect();
        let mut scheduler = Scheduler::new(order, 1_000, TEN_MS);
        let origin = Instant::now();
        let mut starts = Vec::new();

        for tick in 0..=1_000_u64 {
            let now = origin + Duration::from_millis(tick);
            while let Next::Start(address) = scheduler.next(now) {
                starts.push(now);
                scheduler.complete(address);
            }
        }

        let within_first_second = starts
            .iter()
            .filter(|at| **at < origin + Duration::from_secs(1))
            .count();
        assert_eq!(within_first_second, 100);
        assert!(starts.windows(2).all(|pair| pair[1] - pair[0] >= TEN_MS));
    }

    #[test]
    fn address_whose_ip_is_busy_is_skipped_for_the_next_eligible_one() {
        let order = addresses(&["192.0.2.1:8009", "192.0.2.1:8010", "198.51.100.7:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 64, Duration::ZERO);
        let now = Instant::now();

        assert_eq!(scheduler.next(now), Next::Start(order[0]));
        assert_eq!(scheduler.next(now), Next::Start(order[2]));
        assert_eq!(scheduler.next(now), Next::WaitForCompletion);

        scheduler.complete(order[0]);

        assert_eq!(scheduler.next(now), Next::Start(order[1]));
    }

    #[test]
    fn ipv4_and_ipv6_addresses_are_separate_ips() {
        let order =
            addresses(&["127.0.0.1:8009", "[::1]:8009", "[::ffff:127.0.0.1]:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 64, Duration::ZERO);
        let now = Instant::now();

        let first_three: Vec<SocketAddr> =
            (0..3).map(|_| started(scheduler.next(now))).collect();

        assert_eq!(first_three, order);
    }

    #[test]
    fn concurrency_ceiling_holds_new_starts_until_a_completion() {
        let order = addresses(&["192.0.2.1:8009", "192.0.2.2:8009", "192.0.2.3:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 2, Duration::ZERO);
        let now = Instant::now();

        started(scheduler.next(now));
        started(scheduler.next(now));
        assert_eq!(scheduler.next(now), Next::WaitForCompletion);

        scheduler.complete(order[1]);

        assert_eq!(scheduler.next(now), Next::Start(order[2]));
    }

    #[test]
    fn rate_wait_is_reported_only_when_an_address_is_eligible() {
        let order = addresses(&["192.0.2.1:8009", "192.0.2.1:8010"]);
        let mut scheduler = Scheduler::new(order, 64, Duration::from_secs(1));
        let now = Instant::now();

        started(scheduler.next(now));

        assert_eq!(scheduler.next(now), Next::WaitForCompletion);
    }

    #[test]
    fn sweep_finishes_after_the_last_completion() {
        let order = addresses(&["192.0.2.1:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 64, Duration::ZERO);
        let now = Instant::now();

        started(scheduler.next(now));
        assert_eq!(scheduler.next(now), Next::WaitForCompletion);
        scheduler.complete(order[0]);

        assert_eq!(scheduler.next(now), Next::Finished);
    }

    #[test]
    fn empty_sweep_is_finished_at_once() {
        let mut scheduler = Scheduler::new(Vec::new(), 64, TEN_MS);

        assert_eq!(scheduler.next(Instant::now()), Next::Finished);
    }

    #[test]
    fn zero_concurrency_still_lets_one_handshake_run() {
        let order = addresses(&["192.0.2.1:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 0, Duration::ZERO);

        assert_eq!(scheduler.next(Instant::now()), Next::Start(order[0]));
    }

    #[test]
    fn completing_an_unknown_address_changes_nothing() {
        let order = addresses(&["192.0.2.1:8009", "192.0.2.2:8009"]);
        let mut scheduler = Scheduler::new(order.clone(), 1, Duration::ZERO);
        let now = Instant::now();
        started(scheduler.next(now));

        scheduler.complete("203.0.113.9:8009".parse().unwrap());

        assert_eq!(scheduler.next(now), Next::WaitForCompletion);
    }

    #[test]
    fn round_order_is_a_permutation_that_changes_between_rounds() {
        let addresses: Vec<SocketAddr> = (1..=12_u8)
            .map(|host| SocketAddr::from(([192, 0, 2, host], 8009)))
            .collect();
        let mut rng = fastrand::Rng::with_seed(7);

        let first = round_order(&addresses, &mut rng);
        let second = round_order(&addresses, &mut rng);

        let mut sorted_first = first.clone();
        sorted_first.sort();
        assert_eq!(sorted_first, addresses);
        assert_ne!(first, addresses);
        assert_ne!(first, second);
    }

    #[test]
    fn same_seed_gives_the_same_order() {
        let addresses: Vec<SocketAddr> = (1..=12_u8)
            .map(|host| SocketAddr::from(([192, 0, 2, host], 8009)))
            .collect();

        let one = round_order(&addresses, &mut fastrand::Rng::with_seed(3));
        let two = round_order(&addresses, &mut fastrand::Rng::with_seed(3));

        assert_eq!(one, two);
    }
}
