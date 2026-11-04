use super::*;
use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

struct DelayedReady {
    bytes: Cursor<Vec<u8>>,
    now: Rc<Cell<Instant>>,
    delay: Option<Duration>,
    cancel: Option<crate::operation_context::OperationContext>,
}
impl Read for DelayedReady {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if let Some(delay) = self.delay.take() {
            self.now.set(self.now.get() + delay);
            if let Some(context) = self.cancel.take() {
                context.request_cancel().unwrap();
            }
        }
        self.bytes.read(output)
    }
}

#[test]
fn delayed_ready_consumes_the_original_caller_budget_before_body_encoding() {
    let start = Instant::now();
    let now = Rc::new(Cell::new(start));
    let budget = RequestBudget::at(start, Duration::from_millis(100), None).unwrap();
    let mut reader = DelayedReady {
        bytes: Cursor::new(crate::native_request_control::READY.to_vec()),
        now: Rc::clone(&now),
        delay: Some(Duration::from_millis(40)),
        cancel: None,
    };
    read_ready_before(&mut reader, &budget, || now.get(), |_, _| Ok(())).unwrap();
    let remaining = budget.remaining_at(now.get(), Duration::MAX).unwrap();
    let body =
        crate::native_request_control::encode(br#"{"op":"native_status"}"#, remaining).unwrap();
    let (received, _) = crate::native_request_control::decode(&body).unwrap();
    assert_eq!(received, Duration::from_millis(60));
    assert_eq!(budget.deadline, start + Duration::from_millis(100));
}

#[test]
fn expired_or_cancelled_ready_never_permits_the_operation_body() {
    for cancelled in [false, true] {
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let context =
            crate::operation_context::OperationContext::new(start + Duration::from_secs(5));
        let budget =
            RequestBudget::at(start, Duration::from_millis(100), Some(context.clone())).unwrap();
        let mut reader = DelayedReady {
            bytes: Cursor::new(crate::native_request_control::READY.to_vec()),
            now: Rc::clone(&now),
            delay: Some(Duration::from_millis(if cancelled { 1 } else { 101 })),
            cancel: cancelled.then_some(context),
        };
        let mut delivered = Vec::new();
        let ready = read_ready_before(&mut reader, &budget, || now.get(), |_, _| Ok(()));
        if ready.is_ok() {
            write_before(
                &mut delivered,
                b"operation",
                &budget,
                || now.get(),
                |_, _| Ok(()),
            )
            .unwrap();
        }
        assert!(ready.is_err());
        assert!(delivered.is_empty());
    }
}

#[test]
fn unsupported_interim_reply_refuses_body_and_leaves_final_response_unconsumed() {
    let budget = RequestBudget::at(Instant::now(), Duration::from_secs(5), None).unwrap();
    for bytes in [b"HTTP/1.1 200 OK\r\n\r\n".to_vec(), Vec::new()] {
        let mut reader = Cursor::new(bytes);
        assert!(read_ready_before(&mut reader, &budget, Instant::now, |_, _| Ok(())).is_err());
    }
    let mut bytes = crate::native_request_control::READY.to_vec();
    bytes.extend_from_slice(b"HTTP/1.1 200 OK\r\n\r\n{}");
    let mut reader = Cursor::new(bytes);
    read_ready_before(&mut reader, &budget, Instant::now, |_, _| Ok(())).unwrap();
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining).unwrap();
    assert_eq!(remaining, "HTTP/1.1 200 OK\r\n\r\n{}");
}

#[test]
fn scope_probe_reports_exact_request_control_support() {
    use std::net::TcpListener;
    let fixture = super::tests::Fixture::new();
    for capability in [
        Value::Null,
        json!(crate::native_request_control::PROTOCOL),
        json!("unsupported"),
    ] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let registration = Registration::create(&fixture.0, port, "scope_control_secret").unwrap();
        let endpoint = metadata(&fixture.0).unwrap().unwrap();
        let expected = capability == crate::native_request_control::PROTOCOL;
        let value = json!({"project":fixture.0,"pid":endpoint.owner.pid,"port":port,
            "protocol":"aurum-native-host/1","request_control":capability});
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 4096);
            }
            let body = serde_json::to_vec(&value).unwrap();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(&body).unwrap();
        });
        let budget = RequestBudget::at(Instant::now(), Duration::from_secs(5), None).unwrap();
        assert_eq!(
            verify_scope_with_budget(&fixture.0, &endpoint, "scope_control_secret", &budget)
                .unwrap(),
            expected
        );
        worker.join().unwrap();
        drop(registration);
    }
}
