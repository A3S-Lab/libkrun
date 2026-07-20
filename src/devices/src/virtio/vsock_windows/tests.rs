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
