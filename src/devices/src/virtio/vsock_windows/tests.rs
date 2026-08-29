use super::*;

fn tsi_listen_header(dst_port: u32) -> [u8; 44] {
    let mut hdr = [0_u8; 44];
    Vsock::set_u64(&mut hdr, 0, 3);
    Vsock::set_u64(&mut hdr, 8, VSOCK_HOST_CID);
    Vsock::set_u32(&mut hdr, 16, 49_152);
    Vsock::set_u32(&mut hdr, 20, dst_port);
    Vsock::set_u32(&mut hdr, 24, TSI_LISTEN_REQUEST_LEN as u32);
    Vsock::set_u16(&mut hdr, 28, VSOCK_TYPE_DGRAM);
    Vsock::set_u16(&mut hdr, 30, VSOCK_OP_RW);
    hdr
}

fn stream_header(dst_port: u32) -> [u8; 44] {
    let mut hdr = [0_u8; 44];
    Vsock::set_u64(&mut hdr, 0, 3);
    Vsock::set_u64(&mut hdr, 8, VSOCK_HOST_CID);
    Vsock::set_u32(&mut hdr, 16, 49_152);
    Vsock::set_u32(&mut hdr, 20, dst_port);
    Vsock::set_u16(&mut hdr, 28, VSOCK_TYPE_STREAM);
    Vsock::set_u16(&mut hdr, 30, VSOCK_OP_RW);
    hdr
}

#[test]
fn rejects_unsupported_tsi_listener_with_linux_eperm() {
    let mut vsock = Vsock::new(3, None, None, TsiFlags::HIJACK_INET).expect("create vsock");
    let request = tsi_listen_header(TSI_LISTEN_PORT);

    assert!(vsock.reject_unsupported_tsi_listen(&request));

    let response = vsock.pending_rx.pop_front().expect("queued response");
    assert_eq!(Vsock::hdr_u64(&response.hdr, 0), VSOCK_HOST_CID);
    assert_eq!(Vsock::hdr_u64(&response.hdr, 8), 3);
    assert_eq!(Vsock::hdr_u32(&response.hdr, 16), TSI_LISTEN_PORT);
    assert_eq!(Vsock::hdr_u32(&response.hdr, 20), 49_152);
    assert_eq!(Vsock::hdr_u32(&response.hdr, 24), 4);
    assert_eq!(Vsock::hdr_u16(&response.hdr, 28), VSOCK_TYPE_DGRAM);
    assert_eq!(Vsock::hdr_u16(&response.hdr, 30), VSOCK_OP_RW);
    assert_eq!(
        i32::from_le_bytes(response.payload.try_into().expect("four-byte response")),
        -linux_errno_raw(libc::EPERM)
    );
}

#[test]
fn leaves_non_tsi_datagrams_unchanged() {
    let request = tsi_listen_header(TSI_LISTEN_PORT + 1);
    let mut vsock = Vsock::new(3, None, None, TsiFlags::HIJACK_INET).expect("create vsock");

    assert!(!vsock.reject_unsupported_tsi_listen(&request));
    assert!(vsock.pending_rx.is_empty());

    let request = tsi_listen_header(TSI_LISTEN_PORT);
    let mut vsock = Vsock::new(3, None, None, TsiFlags::empty()).expect("create vsock");

    assert!(!vsock.reject_unsupported_tsi_listen(&request));
    assert!(vsock.pending_rx.is_empty());
}

#[test]
fn splits_host_stream_reads_to_fit_guest_rx_descriptors() {
    let request = stream_header(4093);
    let mut vsock = Vsock::new(3, None, None, TsiFlags::empty()).expect("create vsock");
    let payload = (0..MAX_STREAM_RX_CHUNK_BYTES * 2 + 1)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let expected = payload.clone();

    vsock.queue_stream_data(&request, payload);

    let chunks = vsock.pending_rx.iter().collect::<Vec<_>>();
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].payload.len(), MAX_STREAM_RX_CHUNK_BYTES);
    assert_eq!(chunks[1].payload.len(), MAX_STREAM_RX_CHUNK_BYTES);
    assert_eq!(chunks[2].payload.len(), 1);
    for chunk in &chunks {
        assert_eq!(Vsock::hdr_u32(&chunk.hdr, 24) as usize, chunk.payload.len());
        assert_eq!(Vsock::hdr_u16(&chunk.hdr, 30), VSOCK_OP_RW);
    }
    let reconstructed = chunks
        .into_iter()
        .flat_map(|chunk| chunk.payload.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(reconstructed, expected);
}

#[test]
fn drop_stops_and_joins_background_tasks() {
    let stopped = Arc::new(AtomicBool::new(false));
    let task_stopped = stopped.clone();
    let vsock = Vsock::new(3, None, None, TsiFlags::empty()).expect("create vsock");
    vsock.spawn_background_task("vsock-shutdown-test".to_string(), move |shutdown| {
        if wait_for_background_shutdown(shutdown.as_ref(), Duration::from_secs(60)) {
            task_stopped.store(true, Ordering::Release);
        }
    });

    drop(vsock);

    assert!(stopped.load(Ordering::Acquire));
}
