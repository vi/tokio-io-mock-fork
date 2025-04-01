#![warn(rust_2018_idioms)]

use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{Duration, Instant};
use tokio_io_mock_fork::Builder;

#[tokio::test]
async fn read() {
    let mut mock = Builder::new().read(b"hello ").read(b"world!").build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"world!");
}

#[tokio::test]
async fn read_zeroes() {
    let mut mock = Builder::new()
        .read_zeroes(3)
        .read(b"hello")
        .read_zeroes(4)
        .build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"\0\0\0");

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"hello");

    let n = mock.read(&mut buf).await.expect("read 3");
    assert_eq!(&buf[..n], b"\0\0\0\0");
}

#[tokio::test]
async fn read_eof1() {
    let mut mock = Builder::new().read(b"hello ").eof().build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"");
}

#[tokio::test]
async fn read_eof2() {
    let mut mock = Builder::new().read(b"hello ").eof().write(b"qqq").build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"");

    mock.write_all(b"qqq").await.expect("write 1");
}

#[tokio::test]
async fn stop_checking1() {
    let mut mock = Builder::new()
        .read(b"hello ")
        .write(b"qqq")
        .stop_checking()
        .read(b"eee")
        .write(b"444")
        .build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    mock.write_all(b"qqq").await.expect("write 1");
}

#[tokio::test]
#[should_panic]
async fn stop_checking2() {
    let mut mock = Builder::new()
        .read(b"hello ")
        .write(b"qqq")
        .stop_checking()
        .read(b"eee")
        .write(b"444")
        .build();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    mock.write_all(b"qqw").await.expect("write 1");
}

#[tokio::test]
#[should_panic]
async fn stop_checking3() {
    let mut mock = Builder::new()
        .read(b"hello ")
        .write(b"qqq")
        .stop_checking()
        .read(b"eee")
        .write(b"444")
        .build();

    mock.write_all(b"qqq").await.expect("write 1");
}

#[tokio::test]
async fn read_error() {
    let error = io::Error::new(io::ErrorKind::Other, "cruel");
    let mut mock = Builder::new()
        .read(b"hello ")
        .read_error(error)
        .read(b"world!")
        .build();
    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");

    match mock.read(&mut buf).await {
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!("cruel", format!("{error}"));
        }
        Ok(_) => panic!("error not received"),
    }

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"world!");
}

#[tokio::test]
async fn write() {
    let mut mock = Builder::new().write(b"hello ").write(b"world!").build();

    mock.write_all(b"hello ").await.expect("write 1");
    mock.write_all(b"world!").await.expect("write 2");
}

#[tokio::test]
async fn write_zeroes1() {
    let mut mock = Builder::new()
        .write_zeroes(4)
        .write(b"hello ")
        .write_zeroes(3)
        .write(b"world!")
        .write_zeroes(2)
        .build();

    mock.write_all(b"\0\0\0\0").await.expect("write 1");
    mock.write_all(b"hello ").await.expect("write 2");
    mock.write_all(b"\0\0\0").await.expect("write 3");
    mock.write_all(b"world!").await.expect("write 4");
    mock.write_all(b"\0\0").await.expect("write 5");
}

#[tokio::test]
#[should_panic]
async fn write_zeroes2() {
    let mut mock = Builder::new()
        .write_zeroes(4)
        .write(b"hello ")
        .write_zeroes(3)
        .write(b"world!")
        .write_zeroes(2)
        .build();

    mock.write_all(b"\0\0\0\0").await.expect("write 1");
    mock.write_all(b"hello ").await.expect("write 2");
    mock.write_all(b"\0\x44\0").await.expect("write 3");
    mock.write_all(b"world!").await.expect("write 4");
    mock.write_all(b"\0\0").await.expect("write 5");
}

#[tokio::test]
async fn write_ignore() {
    let mut mock = Builder::new()
        .write_zeroes(4)
        .write(b"hello ")
        .write_ignore(3)
        .write(b"world!")
        .write_zeroes(2)
        .build();

    mock.write_all(b"\0\0\0\0").await.expect("write 1");
    mock.write_all(b"hello ").await.expect("write 2");
    mock.write_all(b"\0\x44\0").await.expect("write 3");
    mock.write_all(b"world!").await.expect("write 4");
    mock.write_all(b"\0\0").await.expect("write 5");
}

#[tokio::test]
async fn write_with_handle() {
    let (mut mock, mut handle) = Builder::new().build_with_handle();
    handle.write(b"hello ");
    handle.write(b"world!");

    mock.write_all(b"hello ").await.expect("write 1");
    mock.write_all(b"world!").await.expect("write 2");
}

#[tokio::test]
async fn read_with_handle() {
    let (mut mock, mut handle) = Builder::new().build_with_handle();
    handle.read(b"hello ");
    handle.read(b"world!");

    let mut buf = vec![0; 6];
    mock.read_exact(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..], b"hello ");
    mock.read_exact(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..], b"world!");
}

#[tokio::test]
async fn write_error() {
    let error = io::Error::new(io::ErrorKind::Other, "cruel");
    let mut mock = Builder::new()
        .write(b"hello ")
        .write_error(error)
        .write(b"world!")
        .build();
    mock.write_all(b"hello ").await.expect("write 1");

    match mock.write_all(b"whoa").await {
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!("cruel", format!("{error}"));
        }
        Ok(_) => panic!("error not received"),
    }

    mock.write_all(b"world!").await.expect("write 2");
}

#[tokio::test]
#[should_panic]
async fn mock_panics_read_data_left() {
    use tokio_io_mock_fork::Builder;
    Builder::new().read(b"read").build();
}

#[tokio::test]
#[should_panic]
async fn mock_panics_write_data_left() {
    use tokio_io_mock_fork::Builder;
    Builder::new().write(b"write").build();
}

#[tokio::test(start_paused = true)]
async fn wait() {
    const FIRST_WAIT: Duration = Duration::from_secs(1);

    let mut mock = Builder::new()
        .wait(FIRST_WAIT)
        .read(b"hello ")
        .read(b"world!")
        .build();

    let mut buf = [0; 256];

    let start = Instant::now(); // record the time the read call takes
                                //
    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");
    println!("time elapsed after first read {:?}", start.elapsed());

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"world!");
    println!("time elapsed after second read {:?}", start.elapsed());

    // make sure the .wait() instruction worked
    assert!(
        start.elapsed() >= FIRST_WAIT,
        "consuming the whole mock only took {}ms",
        start.elapsed().as_millis()
    );
}

#[tokio::test(start_paused = true)]
async fn multiple_wait() {
    const FIRST_WAIT: Duration = Duration::from_secs(1);
    const SECOND_WAIT: Duration = Duration::from_secs(1);

    let mut mock = Builder::new()
        .wait(FIRST_WAIT)
        .read(b"hello ")
        .wait(SECOND_WAIT)
        .read(b"world!")
        .build();

    let mut buf = [0; 256];

    let start = Instant::now(); // record the time it takes to consume the mock

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"hello ");
    println!("time elapsed after first read {:?}", start.elapsed());

    let n = mock.read(&mut buf).await.expect("read 2");
    assert_eq!(&buf[..n], b"world!");
    println!("time elapsed after second read {:?}", start.elapsed());

    // make sure the .wait() instruction worked
    assert!(
        start.elapsed() >= FIRST_WAIT + SECOND_WAIT,
        "consuming the whole mock only took {}ms",
        start.elapsed().as_millis()
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn mock_panicless_read_data_left() {
    use tokio_io_mock_fork::Builder;
    let (m, _, p) = Builder::new().read(b"read").build_panicless();
    drop(m);
    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::RemainingUnreadData),
            total_read_bytes: 0,
            total_written_bytes: 0
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn mock_panicless_read_data() {
    use tokio_io_mock_fork::Builder;
    let (mut mock, _, p) = Builder::new().read(b"read").build_panicless();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"read");

    drop(mock);
    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 4,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write(b"world!")
        .build_panicless();

    mock.write_all(b"hello ").await.expect("write 1");
    mock.write_all(b"world!").await.expect("write 2");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 12,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_err() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write(b"world!")
        .build_panicless();

    mock.write_all(b"hello ").await.expect("write 1");
    mock.write_all(b"wor4d!").await.expect("write 2");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::WrittenByteMismatch {
                expected: b'l',
                actual: b'4'
            }),
            total_read_bytes: 0,
            total_written_bytes: 9,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_err2() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write(b"world!")
        .build_panicless();

    mock.write_all(b"hello ").await.expect("write 1");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::RemainingUnwrittenData),
            total_read_bytes: 0,
            total_written_bytes: 6,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_zero() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write_zeroes(3)
        .build_panicless();

    mock.write_all(b"hello \0\0\0").await.expect("write 1");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 9,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_zero_err() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write_zeroes(3)
        .build_panicless();

    mock.write_all(b"hello \0\x05\0").await.expect("write 1");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::WrittenByteMismatch {
                expected: 0,
                actual: 5
            }),
            total_read_bytes: 0,
            total_written_bytes: 7,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_err3() {
    let (mut mock, _, p) = Builder::new()
        .write(b"hello ")
        .write_ignore(3)
        .build_panicless();

    mock.write_all(b"hello \0\x05").await.expect("write 1");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::RemainingUnwrittenData),
            total_read_bytes: 0,
            total_written_bytes: 8,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_unexpected() {
    let (mut mock, _, p) = Builder::new().build_panicless();

    mock.write_all(b"hello").await.expect_err("write 1");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::UnexpectedWrite),
            total_read_bytes: 0,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_unexpected_shutdown() {
    let (mut mock, _, p) = Builder::new()
        .write(b"qqq")
        .enable_shutdown_checking()
        .build_panicless();

    mock.shutdown().await.expect_err("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::ShutdownInsteadOfWrite),
            total_read_bytes: 0,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown() {
    let (mut mock, _, p) = Builder::new()
        .write(b"qqq")
        .enable_shutdown_checking()
        .build_panicless();

    mock.write_all(b"qqq").await.expect("shutdown");
    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 3,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown2() {
    let (mut mock, _, p) = Builder::new().enable_shutdown_checking().build_panicless();

    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown3() {
    let (mut mock, _, p) = Builder::new()
        .read(b"qqq")
        .enable_shutdown_checking()
        .build_panicless();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"qqq");

    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 3,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdow4() {
    let (mut mock, _, p) = Builder::new()
        .write(b"qqq")
        .write_shutdown()
        .build_panicless();

    mock.write_all(b"qqq").await.expect("shutdown");
    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 3,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown5() {
    let (mut mock, _, p) = Builder::new().write_shutdown().build_panicless();

    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 0,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown6() {
    let (mut mock, _, p) = Builder::new()
        .read(b"qqq")
        .write_shutdown()
        .build_panicless();

    let mut buf = [0; 256];

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"qqq");

    mock.shutdown().await.expect("shutdown");
    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 3,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_benign_shutdown7() {
    let (mut mock, _, p) = Builder::new()
        .write_shutdown()
        .read(b"qqq")
        .build_panicless();

    let mut buf = [0; 256];

    mock.shutdown().await.expect("shutdown");

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"qqq");

    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Ok(()),
            total_read_bytes: 3,
            total_written_bytes: 0,
        })
    );
}

#[cfg(feature = "panicless-mode")]
#[tokio::test]
async fn write_panicless_write_instead_of_shutdown() {
    let (mut mock, _, p) = Builder::new()
        .write(b"qqq")
        .write_shutdown()
        .build_panicless();

    mock.write_all(b"qqq").await.expect("write 1");
    mock.write_all(b"wwww").await.expect_err("write 2");

    drop(mock);

    assert_eq!(
        p.await,
        Ok(tokio_io_mock_fork::MockOutcome {
            outcome: Err(tokio_io_mock_fork::MockOutcomeError::WriteInsteadOfShutdown),
            total_read_bytes: 0,
            total_written_bytes: 3,
        })
    );
}

#[tokio::test]
async fn write_shutdown_read() {
    let mut mock = Builder::new()
        .write(b"123")
        .write_shutdown()
        .read(b"qwe")
        .build();

    let mut buf = [0; 256];

    mock.write(b"123").await.expect("write 1");

    mock.shutdown().await.expect("shutdown");

    let n = mock.read(&mut buf).await.expect("read 1");
    assert_eq!(&buf[..n], b"qwe");
}

#[tokio::test]
#[should_panic]
async fn write_shutdown_read_fail() {
    let mut mock = Builder::new()
        .write(b"123")
        .write_shutdown()
        .read(b"qwe")
        .build();

    let mut buf = [0; 256];

    mock.write(b"123").await.expect("write 1");
    mock.read(&mut buf).await.expect_err("read 1");
}
