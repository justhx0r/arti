//! Configuration logic for onion service reverse proxy.

use derive_adhoc::Adhoc;
use derive_builder::Builder;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, ops::RangeInclusive, path::PathBuf, str::FromStr};
//use tor_config::derive_adhoc_template_Flattenable;
use tor_config::{define_list_builder_accessors, define_list_builder_helper, ConfigBuildError};

/// Configuration for a reverse proxy running for one onion service.
#[derive(Clone, Debug, Builder, Eq, PartialEq)]
#[builder(build_fn(error = "ConfigBuildError", validate = "Self::validate"))]
#[builder(derive(Debug, Serialize, Deserialize, Adhoc, Eq, PartialEq))]
#[builder_struct_attr(derive_adhoc(tor_config::Flattenable))]
pub struct ProxyConfig {
    /// A list of rules to apply to incoming requests.  If no rule
    /// matches, we take the DestroyCircuit action.
    #[builder(sub_builder, setter(custom))]
    pub(crate) proxy_ports: ProxyRuleList,
    //
    // TODO: Someday we may want to allow udp, resolve, etc.  If we do, it will
    // be via another option, rather than adding another subtype to ProxySource.
}

impl ProxyConfigBuilder {
    /// Run checks on this ProxyConfig to ensure that it's valid.
    fn validate(&self) -> Result<(), ConfigBuildError> {
        // Make sure that every proxy pattern is actually reachable.
        let mut covered = rangemap::RangeInclusiveSet::<u16>::new();
        for rule in self.proxy_ports.access_opt().iter().flatten() {
            let range = &rule.source.0;
            if covered.gaps(range).next().is_none() {
                return Err(ConfigBuildError::Invalid {
                    field: "proxy_ports".into(),
                    problem: format!("Port pattern {} is not reachable", rule.source),
                });
            }
            covered.insert(range.clone());
        }

        // TODO HSS: Eventually we may want to warn if there are no `Forward`
        // rules, or if the rule list is empty, since this is likely an accident.
        Ok(())
    }
}

define_list_builder_accessors! {
   struct ProxyConfigBuilder {
       pub proxy_ports: [ProxyRule],
   }
}

/// Helper to define builder for ProxyConfig.
type ProxyRuleList = Vec<ProxyRule>;

define_list_builder_helper! {
   #[derive(Eq, PartialEq)]
   pub struct ProxyRuleListBuilder {
       pub(crate) values: [ProxyRule],
   }
   built: ProxyRuleList = values;
   default = vec![];
   item_build: |value| Ok(value.clone());
}

impl ProxyConfig {
    /// Find the configured action to use when receiving a request for a
    /// connection on a given port.
    pub(crate) fn resolve_port_for_begin(&self, port: u16) -> Option<&ProxyAction> {
        self.proxy_ports
            .iter()
            .find(|rule| rule.source.matches_port(port))
            .map(|rule| &rule.target)
    }
}

/// A single rule in a `ProxyConfig`.
///
/// Rules take the form of, "When this pattern matches, take this action."
#[derive(
    Clone,
    Debug,
    // Serialize,
    // Deserialize,
    Eq,
    PartialEq,
    serde_with::DeserializeFromStr,
    serde_with::SerializeDisplay,
)]
// TODO HSS: we might someday want to accept structs here as well, so that
// we can add per-rule fields if we need to.  We can make that an option if/when
// it comes up, however.
// TODO HSS restore this as part of #1058.
//     #[serde(from = "ProxyRuleAsTuple", into = "ProxyRuleAsTuple")]
pub struct ProxyRule {
    /// Any connections to a port matching this pattern match this rule.
    source: ProxyPattern,
    /// When this rule matches, we take this action.
    target: ProxyAction,
}

/*
  TODO HSS restore this code when we do #1058

/// Helper type used to (de)serialize ProxyRule.
type ProxyRuleAsTuple = (ProxyPattern, ProxyAction);
impl From<ProxyRuleAsTuple> for ProxyRule {
    fn from(value: ProxyRuleAsTuple) -> Self {
        Self {
            source: value.0,
            target: value.1,
        }
    }
}
impl From<ProxyRule> for ProxyRuleAsTuple {
    fn from(value: ProxyRule) -> Self {
        (value.source, value.target)
    }
}
*/

// TODO HSS Remove this once we restore the original configuration format in
// #1058. This format/display code is just temporary.
impl std::fmt::Display for ProxyRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} => {}", self.source, self.target)
    }
}
impl FromStr for ProxyRule {
    type Err = ProxyConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (source, target) = s
            .split_once("=>")
            // TODO HSS: This is the wrong error type, but this is throwaway code.
            .ok_or(ProxyConfigError::UnrecognizedTargetType)?;
        Ok(ProxyRule {
            source: source.trim().parse()?,
            target: target.trim().parse()?,
        })
    }
}

impl ProxyRule {
    /// Create a new ProxyRule mapping `source` to `target`.
    pub fn new(source: ProxyPattern, target: ProxyAction) -> Self {
        Self { source, target }
    }
}

/// A set of ports to use when checking how to handle a port.
#[derive(
    Clone, Debug, serde_with::DeserializeFromStr, serde_with::SerializeDisplay, Eq, PartialEq,
)]
pub struct ProxyPattern(RangeInclusive<u16>);

// TODO HSS: Allow ProxyPattern to also be deserialized from an integer.

impl FromStr for ProxyPattern {
    type Err = ProxyConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use ProxyConfigError as PCE;
        if s == "*" {
            Ok(Self::all_ports())
        } else if let Some((left, right)) = s.split_once('-') {
            let left: u16 = left.parse().map_err(PCE::InvalidPort)?;
            let right: u16 = right.parse().map_err(PCE::InvalidPort)?;
            Self::port_range(left, right)
        } else {
            let port = s.parse().map_err(PCE::InvalidPort)?;
            Self::one_port(port)
        }
    }
}
impl std::fmt::Display for ProxyPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.clone().into_inner() {
            (start, end) if start == end => write!(f, "{}", start),
            (1, 65535) => write!(f, "*"),
            (start, end) => write!(f, "{}-{}", start, end),
        }
    }
}

impl ProxyPattern {
    /// Return a pattern matching all ports.
    pub fn all_ports() -> Self {
        Self::check(1, 65535).expect("Somehow, 1-65535 was not a valid pattern")
    }
    /// Return a pattern matching a single port.
    ///
    /// Gives an error if the port is zero.
    pub fn one_port(port: u16) -> Result<Self, ProxyConfigError> {
        Self::check(port, port)
    }
    /// Return a pattern matching all ports between `low` and `high` inclusive.
    ///
    /// Gives an error unless `0 < low <= high`.
    pub fn port_range(low: u16, high: u16) -> Result<Self, ProxyConfigError> {
        Self::check(low, high)
    }

    /// Return true if this pattern includes `port`.
    pub(crate) fn matches_port(&self, port: u16) -> bool {
        self.0.contains(&port)
    }

    /// If start..=end is a valid pattern, wrap it as a ProxyPattern. Otherwise return
    /// an error.
    fn check(start: u16, end: u16) -> Result<ProxyPattern, ProxyConfigError> {
        use ProxyConfigError as PCE;
        match (start, end) {
            (_, 0) => Err(PCE::ZeroPort),
            (0, n) => Ok(Self(1..=n)),
            (low, high) if low > high => Err(PCE::EmptyPortRange),
            (low, high) => Ok(Self(low..=high)),
        }
    }
}

/// An action to take upon receiving an incoming request.
#[derive(
    Clone,
    Debug,
    Default,
    serde_with::DeserializeFromStr,
    serde_with::SerializeDisplay,
    Eq,
    PartialEq,
)]
#[non_exhaustive]
pub enum ProxyAction {
    /// Close the circuit immediately with an error.
    #[default]
    DestroyCircuit,
    /// Accept the client's request and forward it, via some encapsulation method,
    /// to some target address.
    Forward(Encapsulation, TargetAddr),
    /// Close the stream immediately with an error.
    RejectStream,
    /// Ignore the stream request.
    IgnoreStream,
}

/// The address to which we forward an accepted connection.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TargetAddr {
    /// An address that we can reach over the internet.
    //
    // TODO HSS: should we warn if this is a public address?
    Inet(SocketAddr),
    /// An address of a local unix socket.
    Unix(PathBuf),
}

impl FromStr for TargetAddr {
    type Err = ProxyConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use ProxyConfigError as PCE;

        /// Return true if 's' looks like an attempted IPv4 or IPv6 socketaddr.
        fn looks_like_attempted_addr(s: &str) -> bool {
            s.starts_with(|c: char| c.is_ascii_digit())
                || s.strip_prefix('[')
                    .map(|rhs| rhs.starts_with(|c: char| c.is_ascii_hexdigit() || c == ':'))
                    .unwrap_or(false)
        }
        if let Some(path) = s.strip_prefix("unix:") {
            Ok(Self::Unix(PathBuf::from(path)))
        } else if let Some(addr) = s.strip_prefix("inet:") {
            Ok(Self::Inet(addr.parse().map_err(PCE::InvalidTargetAddr)?))
        } else if looks_like_attempted_addr(s) {
            // We check 'looks_like_attempted_addr' before parsing this.
            Ok(Self::Inet(s.parse().map_err(PCE::InvalidTargetAddr)?))
        } else {
            Err(PCE::UnrecognizedTargetType)
        }
    }
}

impl std::fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetAddr::Inet(a) => write!(f, "inet:{}", a),
            TargetAddr::Unix(p) => write!(f, "unix:{}", p.display()),
        }
    }
}

/// The method by which we encapsulate a forwarded request.
///
/// (Right now, only `Simple` is supported, but we may later support
/// "HTTP CONNECT", "HAProxy", or others.)
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Encapsulation {
    /// Handle a request by opening a local socket to the target address and
    /// forwarding the contents verbatim.
    ///
    /// This does not transmit any information about the circuit origin of the request;
    /// only the local port will distinguish one request from another.
    #[default]
    Simple,
}

impl FromStr for ProxyAction {
    type Err = ProxyConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "destroy" {
            Ok(Self::DestroyCircuit)
        } else if s == "reject" {
            Ok(Self::RejectStream)
        } else if s == "ignore" {
            Ok(Self::IgnoreStream)
        } else if let Some(addr) = s.strip_prefix("simple:") {
            Ok(Self::Forward(Encapsulation::Simple, addr.parse()?))
        } else {
            Ok(Self::Forward(Encapsulation::Simple, s.parse()?))
        }
    }
}

impl std::fmt::Display for ProxyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyAction::DestroyCircuit => write!(f, "destroy"),
            ProxyAction::Forward(Encapsulation::Simple, addr) => write!(f, "simple:{}", addr),
            ProxyAction::RejectStream => write!(f, "reject"),
            ProxyAction::IgnoreStream => write!(f, "ignore"),
        }
    }
}

/// An error encountered while parsing or applying a proxy configuration.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ProxyConfigError {
    /// We encountered a proxy target with an unrecognized type keyword.
    #[error("Could not parse proxy target type.")]
    UnrecognizedTargetType,

    /// A socket address could not be parsed to be invalid.
    #[error("Could not parse proxy target address.")]
    InvalidTargetAddr(#[source] std::net::AddrParseError),

    /// A socket rule had an source port that couldn't be parsed as a `u16`.
    #[error("Could not parse proxy source port.")]
    InvalidPort(#[source] std::num::ParseIntError),

    /// A socket rule had a zero source port.
    #[error("Zero is not a valid port.")]
    ZeroPort,

    /// A socket rule specified an empty port range.
    #[error("Port range is empty.")]
    EmptyPortRange,
}

#[cfg(test)]
mod test {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    #![allow(clippy::needless_pass_by_value)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->
    use super::*;

    #[test]
    fn pattern_ok() {
        use ProxyPattern as P;
        assert_eq!(P::from_str("*").unwrap(), P(1..=65535));
        assert_eq!(P::from_str("100").unwrap(), P(100..=100));
        assert_eq!(P::from_str("100-200").unwrap(), P(100..=200));
        assert_eq!(P::from_str("0-200").unwrap(), P(1..=200));
    }

    #[test]
    fn pattern_display() {
        use ProxyPattern as P;
        assert_eq!(P::all_ports().to_string(), "*");
        assert_eq!(P::one_port(100).unwrap().to_string(), "100");
        assert_eq!(P::port_range(100, 200).unwrap().to_string(), "100-200");
    }

    #[test]
    fn pattern_err() {
        use ProxyConfigError as PCE;
        use ProxyPattern as P;
        assert!(matches!(P::from_str("fred"), Err(PCE::InvalidPort(_))));
        assert!(matches!(P::from_str("100-fred"), Err(PCE::InvalidPort(_))));
        assert!(matches!(P::from_str("100-42"), Err(PCE::EmptyPortRange)));
    }

    #[test]
    fn target_ok() {
        use Encapsulation::Simple;
        use ProxyAction as T;
        use TargetAddr as A;
        assert!(matches!(T::from_str("reject"), Ok(T::RejectStream)));
        assert!(matches!(T::from_str("ignore"), Ok(T::IgnoreStream)));
        assert!(matches!(T::from_str("destroy"), Ok(T::DestroyCircuit)));
        let sa: SocketAddr = "192.168.1.1:50".parse().unwrap();
        assert!(
            matches!(T::from_str("192.168.1.1:50"), Ok(T::Forward(Simple, A::Inet(a))) if a == sa)
        );
        assert!(
            matches!(T::from_str("inet:192.168.1.1:50"), Ok(T::Forward(Simple, A::Inet(a))) if a == sa)
        );
        let sa: SocketAddr = "[::1]:999".parse().unwrap();
        assert!(matches!(T::from_str("[::1]:999"), Ok(T::Forward(Simple, A::Inet(a))) if a == sa));
        assert!(
            matches!(T::from_str("inet:[::1]:999"), Ok(T::Forward(Simple, A::Inet(a))) if a == sa)
        );
        let pb = PathBuf::from("/var/run/hs/socket");
        assert!(
            matches!(T::from_str("unix:/var/run/hs/socket"), Ok(T::Forward(Simple, A::Unix(p))) if p == pb)
        );
    }

    #[test]
    fn target_display() {
        use Encapsulation::Simple;
        use ProxyAction as T;
        use TargetAddr as A;

        assert_eq!(T::RejectStream.to_string(), "reject");
        assert_eq!(T::IgnoreStream.to_string(), "ignore");
        assert_eq!(T::DestroyCircuit.to_string(), "destroy");
        assert_eq!(
            T::Forward(Simple, A::Inet("192.168.1.1:50".parse().unwrap())).to_string(),
            "simple:inet:192.168.1.1:50"
        );
        assert_eq!(
            T::Forward(Simple, A::Inet("[::1]:999".parse().unwrap())).to_string(),
            "simple:inet:[::1]:999"
        );
        assert_eq!(
            T::Forward(Simple, A::Unix("/var/run/hs/socket".into())).to_string(),
            "simple:unix:/var/run/hs/socket"
        );
    }

    #[test]
    fn target_err() {
        use ProxyAction as T;
        use ProxyConfigError as PCE;

        assert!(matches!(
            T::from_str("sdakljf"),
            Err(PCE::UnrecognizedTargetType)
        ));

        assert!(matches!(
            T::from_str("inet:hello"),
            Err(PCE::InvalidTargetAddr(_))
        ));
        assert!(matches!(
            T::from_str("inet:wwww.example.com:80"),
            Err(PCE::InvalidTargetAddr(_))
        ));

        assert!(matches!(
            T::from_str("127.1:80"),
            Err(PCE::InvalidTargetAddr(_))
        ));
        assert!(matches!(
            T::from_str("inet:127.1:80"),
            Err(PCE::InvalidTargetAddr(_))
        ));
        assert!(matches!(
            T::from_str("127.1:80"),
            Err(PCE::InvalidTargetAddr(_))
        ));
        assert!(matches!(
            T::from_str("inet:2130706433:80"),
            Err(PCE::InvalidTargetAddr(_))
        ));

        assert!(matches!(
            T::from_str("128.256.cats.and.dogs"),
            Err(PCE::InvalidTargetAddr(_))
        ));
    }

    #[test]
    fn deserialize() {
        use Encapsulation::Simple;
        use TargetAddr as A;
        /* TODO HSS #1058 restore this.
        let ex = r#"{
            "proxy_ports": [
                [ "443", "127.0.0.1:11443" ],
                [ "80", "ignore" ],
                [ "*", "destroy" ]
            ]
        }"#; */
        let ex = r#"{
            "proxy_ports": [
                "443 => 127.0.0.1:11443",
                "80 => ignore",
                "* => destroy"
            ]
        }"#;
        let bld: ProxyConfigBuilder = serde_json::from_str(ex).unwrap();
        let cfg = bld.build().unwrap();
        assert_eq!(cfg.proxy_ports.len(), 3);
        assert_eq!(cfg.proxy_ports[0].source.0, 443..=443);
        assert_eq!(cfg.proxy_ports[1].source.0, 80..=80);
        assert_eq!(cfg.proxy_ports[2].source.0, 1..=65535);

        assert_eq!(
            cfg.proxy_ports[0].target,
            ProxyAction::Forward(Simple, A::Inet("127.0.0.1:11443".parse().unwrap()))
        );
        assert_eq!(cfg.proxy_ports[1].target, ProxyAction::IgnoreStream);
        assert_eq!(cfg.proxy_ports[2].target, ProxyAction::DestroyCircuit);
    }

    #[test]
    fn validation_fail() {
        // this should fail; the third pattern isn't reachable.
        /* TODO HSS #1058 restore this.
        let ex = r#"{
            "proxy_ports": [
                [ "2-300", "127.0.0.1:11443" ],
                [ "301-999", "ignore" ],
                [ "30-310", "destroy" ]
            ]
        }"#; */
        let ex = r#"{
            "proxy_ports": [
                "2-300 => 127.0.0.1:11443",
                "301-999 => ignore",
                "30-310 => destroy"
            ]
        }"#;
        let bld: ProxyConfigBuilder = serde_json::from_str(ex).unwrap();
        match bld.build() {
            Err(ConfigBuildError::Invalid { field, problem }) => {
                assert_eq!(field, "proxy_ports");
                assert_eq!(problem, "Port pattern 30-310 is not reachable");
            }
            other => panic!("Expected an Invalid error; got {other:?}"),
        }

        // This should work; the third pattern is not completely covered.
        /* TODO HSS #1058 restore this.
        let ex = r#"{
            "proxy_ports": [
                [ "2-300", "127.0.0.1:11443" ],
                [ "302-999", "ignore" ],
                [ "30-310", "destroy" ]
            ]
        }"#; */
        let ex = r#"{
            "proxy_ports": [
                "2-300 => 127.0.0.1:11443",
                "302-999 => ignore",
                "30-310 => destroy"
            ]
        }"#;
        let bld: ProxyConfigBuilder = serde_json::from_str(ex).unwrap();
        assert!(bld.build().is_ok());
    }

    #[test]
    fn demo() {
        let b: ProxyConfigBuilder = toml::de::from_str(
            /* TODO HSS #1058 restore this.
                        r#"
            proxy_ports = [
                ["80", "127.0.0.1:10080"],
                ["22", "destroy"],
                ["265", "ignore"],
                ["1-1024", "unix:/var/run/allium-cepa/socket"],
            ]
            "#, */
            r#"
proxy_ports = [
    "80 => 127.0.0.1:10080",
    "22 => destroy",
    "265 => ignore",
    "1-1024 => unix:/var/run/allium-cepa/socket",
]
"#,
        )
        .unwrap();
        let c = b.build().unwrap();
        assert_eq!(c.proxy_ports.len(), 4);
        // TODO HSS: test actual contents.
    }
}
