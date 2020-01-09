use ::pnet_bandwhich_fork::datalink::Channel::Ethernet;
use ::pnet_bandwhich_fork::datalink::DataLinkReceiver;
use ::pnet_bandwhich_fork::datalink::{self, Config, NetworkInterface};
use ::std::io::{self, stdin, Write};
use ::termion::event::Event;
use ::termion::input::TermRead;
use ::tokio::runtime::Runtime;

use ::std::io::ErrorKind;
use ::std::time;

use signal_hook::iterator::Signals;
use failure::{Context, Fail, Backtrace};


#[cfg(target_os = "linux")]
use crate::os::linux::get_open_sockets;
#[cfg(target_os = "macos")]
use crate::os::macos::get_open_sockets;
use crate::{network::dns, OsInputOutput};

pub type OnSigWinch = dyn Fn(Box<dyn Fn()>) + Send;
pub type SigCleanup = dyn Fn() + Send;

pub struct KeyboardEvents;

impl Iterator for KeyboardEvents {
    type Item = Event;
    fn next(&mut self) -> Option<Event> {
        match stdin().events().next() {
            Some(Ok(ev)) => Some(ev),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct MyError {
    inner: Context<MyErrorKind>,
}
impl MyError {
    pub fn kind(&self) -> MyErrorKind {
        *self.inner.get_context()
    }
}

impl From<MyErrorKind> for MyError {
    fn from(kind: MyErrorKind) -> MyError {
        MyError { inner: Context::new(kind) }
    }
}

impl From<Context<MyErrorKind>> for MyError {
    fn from(inner: Context<MyErrorKind>) -> MyError {
        MyError { inner: inner }
    }
}
#[derive(Copy, Clone, Eq, PartialEq, Debug, Fail)]
enum MyErrorKind {
    #[fail(display = "Custom error message")]
    Networkfail,
}
impl Fail for MyError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.inner.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.inner.backtrace()
    }
}

impl fmt::Display for MyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

// A sample function to produce an AppError Err
fn produce_error() -> Result<(), MyError> {
    Err(MyError::from(MyErrorKind::Networkfail))
}

fn get_datalink_channel(
    interface: &NetworkInterface,
) -> Result<Box<dyn DataLinkReceiver>, std::io::Error> {
    let mut config = Config::default();
    config.read_timeout = Some(time::Duration::new(1, 0));

    match datalink::channel(interface, config) {
        Ok(Ethernet(_tx, rx)) => Ok(rx),
        Ok(_) => Err(MyError::from(MyErrorKind::Networkfail)),
        Err(e) => Err(e),
    }
}

fn get_interface(interface_name: &str) -> Option<NetworkInterface> {
    datalink::interfaces()
        .into_iter()
        .find(|iface| iface.name == interface_name)
}

fn sigwinch() -> (Box<OnSigWinch>, Box<SigCleanup>) {
    let signals = Signals::new(&[signal_hook::SIGWINCH]).unwrap();
    let on_winch = {
        let signals = signals.clone();
        move |cb: Box<dyn Fn()>| {
            for signal in signals.forever() {
                match signal {
                    signal_hook::SIGWINCH => cb(),
                    _ => unreachable!(),
                }
            }
        }
    };
    let cleanup = move || {
        signals.close();
    };
    (Box::new(on_winch), Box::new(cleanup))
}

fn create_write_to_stdout() -> Box<dyn FnMut(String) + Send> {
    Box::new({
        let mut stdout = io::stdout();
        move |output: String| {
            writeln!(stdout, "{}", output).unwrap();
        }
    })
}

pub fn get_input(
    interface_name: &Option<String>,
    resolve: bool,
) -> Result<OsInputOutput, failure::Error> {
    let network_interfaces = if let Some(name) = interface_name {
        match get_interface(&name) {
            Some(interface) => vec![interface],
            None => {
                failure::bail!("Cannot find interface {}", name);
                // the homebrew formula relies on this wording, please be careful when changing
            }
        }
    } else {
        datalink::interfaces()
    };

    let network_frames = network_interfaces
        .iter()
        .map(|iface| get_datalink_channel(iface));

    let available_network_frames = network_frames
        .clone()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();

    if available_network_frames.is_empty() {
        for iface in network_frames {
            if let Some(iface_error) = iface.err() {
                if let ErrorKind::PermissionDenied = iface_error.kind() {
                    failure::bail!(eperm_message())
                }
            }
        }
        failure::bail!("Failed to find any network interface to listen on.");
    }

    let keyboard_events = Box::new(KeyboardEvents);
    let write_to_stdout = create_write_to_stdout();
    let (on_winch, cleanup) = sigwinch();
    let dns_client = if resolve {
        let mut runtime = Runtime::new()?;
        let resolver = runtime.block_on(dns::Resolver::new(runtime.handle().clone()))?;
        let dns_client = dns::Client::new(resolver, runtime)?;
        Some(dns_client)
    } else {
        None
    };

    Ok(OsInputOutput {
        network_interfaces,
        network_frames: available_network_frames,
        get_open_sockets,
        keyboard_events,
        dns_client,
        on_winch,
        cleanup,
        write_to_stdout,
    })
}

#[inline]
#[cfg(target_os = "macos")]
fn eperm_message() -> &'static str {
    "Insufficient permissions to listen on network interface(s). Try running with sudo."
}

#[inline]
#[cfg(target_os = "linux")]
fn eperm_message() -> &'static str {
    r#"
    Insufficient permissions to listen on network interface(s). You can work around
    this issue like this:

    * Try running `bandwhich` with `sudo`

    * Build a `setcap(8)` wrapper for `bandwhich` with the following rules:
        `cap_net_raw,cap_net_admin+ep`
    "#
}
