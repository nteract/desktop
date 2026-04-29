use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex as StdMutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use notebook_protocol::protocol::KernelPorts;
use tokio::net::TcpListener;
use tracing::{debug, warn};

pub const DEFAULT_KERNEL_PORT_RANGE: RangeInclusive<u16> = 9000..=29999;
pub const TEST_KERNEL_PORT_RANGE_SIZE: u16 = 1000;
pub const MAX_KERNEL_PORT_LAUNCH_ATTEMPTS: usize = 4;

const PORTS_PER_KERNEL: u16 = 5;
static NEXT_KERNEL_PORT_BLOCK: AtomicU64 = AtomicU64::new(0);
static RESERVED_KERNEL_PORTS: OnceLock<StdMutex<HashSet<u16>>> = OnceLock::new();

fn reservations() -> &'static StdMutex<HashSet<u16>> {
    RESERVED_KERNEL_PORTS.get_or_init(|| StdMutex::new(HashSet::new()))
}

fn lock_reservations() -> MutexGuard<'static, HashSet<u16>> {
    match reservations().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Debug)]
pub struct KernelPortReservation {
    ports: KernelPorts,
}

impl KernelPortReservation {
    pub fn ports(&self) -> KernelPorts {
        self.ports
    }
}

impl Drop for KernelPortReservation {
    fn drop(&mut self) {
        release_ports(kernel_ports_array(self.ports));
    }
}

pub async fn reserve_kernel_ports() -> Result<KernelPortReservation> {
    reserve_kernel_ports_for_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).await
}

pub(crate) fn is_kernel_port_bind_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("address already in use")
        || lower.contains("address in use")
        || lower.contains("eaddrinuse")
        || lower.contains("wsaeaddrinuse")
        || lower.contains("winerror 10048")
        || lower.contains("errno = 10048")
        || lower.contains("os error 48")
        || lower.contains("os error 98")
        || lower.contains("os error 10048")
}

async fn reserve_kernel_ports_for_ip(ip: IpAddr) -> Result<KernelPortReservation> {
    let range = kernel_port_range();
    let blocks = candidate_blocks(range.clone());
    reserve_from_blocks(ip, blocks)
        .await
        .with_context(|| format!("no available kernel port allocation in range {range:?}"))
}

fn kernel_port_range() -> RangeInclusive<u16> {
    match std::env::var("RUNTIMED_TEST_KERNEL_PORT_RANGE_START") {
        Ok(raw) => match raw.parse::<u16>() {
            Ok(start) => test_kernel_port_range(start),
            Err(e) => {
                warn!(
                    "[kernel-ports] Ignoring invalid RUNTIMED_TEST_KERNEL_PORT_RANGE_START={raw:?}: {e}"
                );
                DEFAULT_KERNEL_PORT_RANGE
            }
        },
        Err(_) => DEFAULT_KERNEL_PORT_RANGE,
    }
}

fn test_kernel_port_range(start: u16) -> RangeInclusive<u16> {
    let end = start.saturating_add(TEST_KERNEL_PORT_RANGE_SIZE - 1);
    start..=end
}

fn candidate_blocks(range: RangeInclusive<u16>) -> Vec<[u16; PORTS_PER_KERNEL as usize]> {
    let start = u32::from(*range.start());
    let end = u32::from(*range.end());
    let len = end.saturating_sub(start) + 1;
    let block_count = len / u32::from(PORTS_PER_KERNEL);
    if block_count == 0 {
        return Vec::new();
    }

    let offset = NEXT_KERNEL_PORT_BLOCK.fetch_add(1, Ordering::Relaxed) as u32 % block_count;
    (0..block_count)
        .filter_map(|i| {
            let block_start = start + ((offset + i) % block_count) * u32::from(PORTS_PER_KERNEL);
            let block = [
                u16::try_from(block_start).ok()?,
                u16::try_from(block_start + 1).ok()?,
                u16::try_from(block_start + 2).ok()?,
                u16::try_from(block_start + 3).ok()?,
                u16::try_from(block_start + 4).ok()?,
            ];
            Some(block)
        })
        .collect()
}

async fn reserve_from_blocks(
    ip: IpAddr,
    blocks: Vec<[u16; PORTS_PER_KERNEL as usize]>,
) -> Result<KernelPortReservation> {
    let mut last_bind_error = None;

    for block in blocks {
        if !claim_ports(block) {
            continue;
        }

        match probe_ports(ip, block).await {
            Ok(()) => {
                let ports = KernelPorts {
                    stdin: block[0],
                    control: block[1],
                    hb: block[2],
                    shell: block[3],
                    iopub: block[4],
                };
                debug!("[kernel-ports] Reserved kernel ports: {:?}", ports);
                return Ok(KernelPortReservation { ports });
            }
            Err(e) => {
                release_ports(block);
                last_bind_error = Some(e);
            }
        }
    }

    if let Some(e) = last_bind_error {
        Err(e).context("all candidate kernel port blocks were unavailable")
    } else {
        anyhow::bail!("all candidate kernel port blocks are already reserved")
    }
}

fn claim_ports(ports: [u16; PORTS_PER_KERNEL as usize]) -> bool {
    let mut reserved = lock_reservations();
    if ports.iter().any(|port| reserved.contains(port)) {
        return false;
    }
    for port in ports {
        reserved.insert(port);
    }
    true
}

fn release_ports(ports: [u16; PORTS_PER_KERNEL as usize]) {
    let mut reserved = lock_reservations();
    for port in ports {
        reserved.remove(&port);
    }
}

async fn probe_ports(ip: IpAddr, ports: [u16; PORTS_PER_KERNEL as usize]) -> Result<()> {
    let mut listeners = Vec::with_capacity(PORTS_PER_KERNEL as usize);
    for port in ports {
        listeners.push(TcpListener::bind(SocketAddr::new(ip, port)).await?);
    }
    drop(listeners);
    Ok(())
}

fn kernel_ports_array(ports: KernelPorts) -> [u16; PORTS_PER_KERNEL as usize] {
    [
        ports.stdin,
        ports.control,
        ports.hb,
        ports.shell,
        ports.iopub,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(start: u16) -> [u16; PORTS_PER_KERNEL as usize] {
        [start, start + 1, start + 2, start + 3, start + 4]
    }

    fn reserved_contains_any(ports: [u16; PORTS_PER_KERNEL as usize]) -> bool {
        let reserved = reservations()
            .lock()
            .expect("kernel port reservations poisoned");
        ports.iter().any(|port| reserved.contains(port))
    }

    #[tokio::test]
    async fn concurrent_allocations_from_same_daemon_do_not_overlap() {
        let first = reserve_from_blocks(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            vec![block(19000), block(19005)],
        )
        .await
        .expect("first allocation");
        let second = reserve_from_blocks(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            vec![block(19000), block(19005)],
        )
        .await
        .expect("second allocation");

        let first_ports: HashSet<_> = kernel_ports_array(first.ports()).into_iter().collect();
        let second_ports: HashSet<_> = kernel_ports_array(second.ports()).into_iter().collect();
        assert!(first_ports.is_disjoint(&second_ports));
    }

    #[tokio::test]
    async fn failed_partial_allocation_releases_all_claimed_ports() {
        let occupied = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19012))
            .await
            .expect("occupy port");
        let failed_block = block(19010);

        let result = reserve_from_blocks(IpAddr::V4(Ipv4Addr::LOCALHOST), vec![failed_block]).await;
        assert!(result.is_err());
        assert!(!reserved_contains_any(failed_block));

        drop(occupied);
    }

    #[tokio::test]
    async fn occupied_ports_are_skipped() {
        let occupied = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 19022))
            .await
            .expect("occupy port");

        let reservation = reserve_from_blocks(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            vec![block(19020), block(19025)],
        )
        .await
        .expect("allocation should skip occupied block");

        assert_eq!(reservation.ports().stdin, 19025);
        drop(occupied);
    }

    #[tokio::test]
    async fn successful_reservation_releases_on_drop() {
        let reusable_block = block(19030);
        let reservation =
            reserve_from_blocks(IpAddr::V4(Ipv4Addr::LOCALHOST), vec![reusable_block])
                .await
                .expect("reservation");
        assert!(reserved_contains_any(reusable_block));

        drop(reservation);
        assert!(!reserved_contains_any(reusable_block));

        let second = reserve_from_blocks(IpAddr::V4(Ipv4Addr::LOCALHOST), vec![reusable_block])
            .await
            .expect("second reservation should reuse dropped block");
        assert_eq!(second.ports().stdin, reusable_block[0]);
    }

    #[test]
    fn test_range_slicing_uses_1000_port_window() {
        assert_eq!(test_kernel_port_range(9000), 9000..=9999);
        assert_eq!(test_kernel_port_range(14000), 14000..=14999);
    }

    #[test]
    fn bind_error_matcher_covers_kernel_error_text() {
        assert!(is_kernel_port_bind_error(
            "Failed to launch kernel: Address already in use (os error 48)"
        ));
        assert!(is_kernel_port_bind_error(
            "zmq.error.ZMQError: Address already in use"
        ));
        assert!(is_kernel_port_bind_error(
            "OSError: [WinError 10048] Only one usage of each socket address is normally permitted"
        ));
        assert!(is_kernel_port_bind_error(
            "zmq.error.ZMQError: bind failed with errno = WSAEADDRINUSE"
        ));
        assert!(is_kernel_port_bind_error(
            "zmq.error.ZMQError: bind failed with errno = 10048"
        ));
        assert!(!is_kernel_port_bind_error(
            "Failed to launch kernel: ModuleNotFoundError: No module named ipykernel"
        ));
    }
}
