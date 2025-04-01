# tokio-io-mock-fork

Forked and extended [`tokio_test::io`](https://docs.rs/tokio-test/latest/tokio_test/io/index.html) module.
Like original module, it allows creating `AsyncRead` + `AsyncWrite` objects which behave according to
scenarios set using builder API.

## Additional Features

* More informative panic messages (can include name, number of read and written bytes, etc.)
* More actions to mock: read EOFs, write shutdowns
* Ability to skip checking some of the written bytes; ability to turn off assertions entirely after a certain point.
* Panicless mode where failures are signaled using a channel instead.
* Dedicated tools to work with large spans of zero bytes (without keeping them allocated in memory).
* Text scenarios - a concise way to schedule multiple different actions using one `&str`.


## Example

```rust
let mut mock = Builder::new().read(b"ping").eof().write(b"pong").build();

let mut buf = [0; 256];

let n = mock.read(&mut buf).await?;
assert_eq!(&buf[..n], b"ping");

let n = mock.read(&mut buf).await?;
assert_eq!(n, 0);

mock.write_all(b"pong").await?;
```

