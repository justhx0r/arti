//! Principal types for onion services.
#![allow(dead_code, unused_variables)] // TODO hss remove.
pub(crate) mod netdir;

use std::sync::{Arc, Mutex};

use futures::channel::mpsc;
use futures::Stream;
use safelog::sensitive;
use tor_async_utils::oneshot;
use tor_async_utils::PostageWatchSenderExt as _;
use tor_circmgr::hspool::HsCircPool;
use tor_config::{Reconfigure, ReconfigureError};
use tor_error::Bug;
use tor_hscrypto::pk::HsId;
use tor_hscrypto::pk::HsIdKey;
use tor_hscrypto::pk::HsIdKeypair;
use tor_keymgr::KeyMgr;
use tor_keymgr::KeystoreSelector;
use tor_llcrypto::pk::curve25519;
use tor_llcrypto::pk::ed25519;
use tor_netdir::NetDirProvider;
use tor_rtcompat::Runtime;
use tracing::{info, trace, warn};

use crate::ipt_mgr::IptManager;
use crate::ipt_set::IptsManagerView;
use crate::svc::publish::Publisher;
use crate::HsIdKeypairSpecifier;
use crate::HsIdPublicKeySpecifier;
use crate::HsNickname;
use crate::OnionServiceConfig;
use crate::OnionServiceStatus;
use crate::RendRequest;
use crate::StartupError;

pub(crate) mod ipt_establish;
pub(crate) mod publish;
pub(crate) mod rend_handshake;

/// Convenience alias for link specifiers of an intro point
pub(crate) type LinkSpecs = Vec<tor_linkspec::EncodedLinkSpec>;

/// Convenient type alias for an ntor public key
// TODO HSS maybe this should be `tor_proto::crypto::handshake::ntor::NtorPublicKey`?
type NtorPublicKey = curve25519::PublicKey;

/// A handle to an instance of an onion service.
//
// TODO HSS: Write more.
//
// (APIs should return Arc<OnionService>)
#[must_use = "a hidden service object will terminate the service when dropped"]
pub struct OnionService {
    /// The mutable implementation details of this onion service.
    inner: Mutex<SvcInner>,
}

/// Implementation details for an onion service.
struct SvcInner {
    /// Configuration information about this service.
    config_tx: postage::watch::Sender<Arc<OnionServiceConfig>>,

    /// A keymgr used to look up our keys and store new medium-term keys.
    //
    // TODO HSS: Do we actually need this in this structure?
    keymgr: Arc<KeyMgr>,

    /// A oneshot that will be dropped when this object is dropped.
    shutdown_tx: oneshot::Sender<void::Void>,

    /// Handles that we'll take ownership of when launching the service.
    ///
    /// (TODO HSS: Having to consume this may indicate a design problem.)
    unlaunched: Option<(
        mpsc::Receiver<RendRequest>,
        Box<dyn Launchable + Send + Sync>,
    )>,
}

/// Objects and handles needed to launch an onion service.
struct ForLaunch<R: Runtime> {
    /// An unlaunched handle for the HsDesc publisher.
    ///
    /// This publisher is responsible for determining when we need to upload a
    /// new set of HsDescs, building them, and publishing them at the correct
    /// HsDirs.
    publisher: Publisher<R, publish::Real<R>>,

    /// Our handler for the introduction point manager.
    ///
    /// This manager is responsible for selecting introduction points,
    /// maintaining our connections to them, and telling the publisher which ones
    /// are publicly available.
    ipt_mgr: IptManager<R, crate::ipt_mgr::Real<R>>,

    /// A handle used by the ipt manager to send Ipts to the publisher.
    ///
    ///
    ipt_mgr_view: IptsManagerView,
}

/// Private trait used to type-erase `ForLaunch<R>`, so that we don't need to
/// parameterize OnionService on `<R>`.
trait Launchable: Send + Sync {
    /// Launch
    fn launch(self: Box<Self>) -> Result<(), StartupError>;
}

impl<R: Runtime> Launchable for ForLaunch<R> {
    fn launch(self: Box<Self>) -> Result<(), StartupError> {
        self.ipt_mgr.launch_background_tasks(self.ipt_mgr_view)?;
        self.publisher.launch()?;
        Ok(())
    }
}

impl OnionService {
    /// Create (but do not launch) a new onion service.
    //
    // TODO HSS: How do we handle the case where somebody tries to launch two
    // onion services with the same nickname?  They will conflict by trying to
    // use the same state and the same keys.  Do we stop it here, or in
    // arti_client?
    pub fn new<R, S>(
        runtime: R,
        config: OnionServiceConfig,
        netdir_provider: Arc<dyn NetDirProvider>,
        circ_pool: Arc<HsCircPool<R>>,
        keymgr: Arc<KeyMgr>,
        statemgr: S,
    ) -> Result<Arc<Self>, StartupError>
    where
        R: Runtime,
        S: tor_persist::StateMgr + Send + Sync + 'static,
    {
        let nickname = config.nickname.clone();
        // TODO HSS: Maybe, adjust tor_persist::fs to handle subdirectories, and
        // use onion/{nickname}?
        let storage_key = format!("onion_svc_{nickname}");
        // TODO HSS-IPT-PERSIST: Use this handle, and use a real struct type instead.
        let storage_handle: Arc<dyn tor_persist::StorageHandle<()>> =
            statemgr.create_handle(storage_key);

        let (rend_req_tx, rend_req_rx) = mpsc::channel(32);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (config_tx, config_rx) = postage::watch::channel_with(Arc::new(config));

        // TODO HSS: How do I give ipt_mgr_view to ipt_mgr?  Does IptManager even take
        //          one of these?
        let (ipt_mgr_view, publisher_view) = crate::ipt_set::ipts_channel(None);

        let ipt_mgr = IptManager::new(
            runtime.clone(),
            netdir_provider.clone(),
            nickname.clone(),
            config_rx.clone(),
            rend_req_tx,
            shutdown_rx,
            crate::ipt_mgr::Real {
                circ_pool: circ_pool.clone(),
            },
            keymgr.clone(),
        )?;

        // TODO HSS: add a config option for specifying whether to expect the KS_hsid to be stored
        // offline
        //let offline_hsid = config.offline_hsid;
        let offline_hsid = false;

        maybe_generate_hsid(&keymgr, &nickname, offline_hsid)?;

        let publisher: Publisher<R, publish::Real<R>> = Publisher::new(
            runtime,
            nickname,
            netdir_provider,
            circ_pool,
            publisher_view,
            config_rx,
            Arc::clone(&keymgr),
        );

        // TODO HSS: we need to actually do something with: shutdown_tx,
        // rend_req_rx.  The latter may need to be refactored to actually work
        // with svc::rend_handshake, if it doesn't already.

        Ok(Arc::new(OnionService {
            inner: Mutex::new(SvcInner {
                config_tx,
                shutdown_tx,
                keymgr,
                unlaunched: Some((
                    rend_req_rx,
                    Box::new(ForLaunch {
                        publisher,
                        ipt_mgr,
                        ipt_mgr_view,
                    }),
                )),
            }),
        }))
    }

    /// Change the configuration of this onion service.
    ///
    /// (Not everything can be changed here. At the very least we'll need to say
    /// that the identity of a service is fixed. We might want to make the
    /// storage  backing this, and the anonymity status, unchangeable.)
    pub fn reconfigure(
        &self,
        new_config: OnionServiceConfig,
        how: Reconfigure,
    ) -> Result<(), ReconfigureError> {
        let mut inner = self.inner.lock().expect("lock poisoned");
        inner.config_tx.try_maybe_send(|cur_config| {
            let new_config = cur_config.for_transition_to(new_config, how)?;
            Ok(match how {
                // We're only checking, so return the current configuration.
                tor_config::Reconfigure::CheckAllOrNothing => Arc::clone(cur_config),
                // We're replacing the configuration, and we didn't get an error.
                _ => Arc::new(new_config),
            })
        })

        // TODO HSS: We need to make sure that the various tasks listening on
        // config_rx actually enforce the configuration, not only on new
        // connections, but existing ones.
    }

    /// Tell this onion service about some new short-term keys it can use.
    pub fn add_keys(&self, keys: ()) -> Result<(), Bug> {
        todo!() // TODO hss
    }

    /// Return the current status of this onion service.
    pub fn status(&self) -> OnionServiceStatus {
        todo!() // TODO hss
    }

    // TODO hss let's also have a function that gives you a stream of Status
    // changes?  Or use a publish-based watcher?

    /// Tell this onion service to begin running, and return a
    /// stream of rendezvous requests on the service.
    ///
    /// You can turn the resulting stream into a stream of [`StreamRequest`](crate::StreamRequest)
    /// using the [`handle_rend_requests`](crate::handle_rend_requests) helper function
    pub fn launch(self: &Arc<Self>) -> Result<impl Stream<Item = RendRequest>, StartupError> {
        let (rend_req_rx, launch) = {
            let mut inner = self.inner.lock().expect("poisoned lock");
            inner
                .unlaunched
                .take()
                .ok_or(StartupError::AlreadyLaunched)?
        };

        launch.launch()?;

        // TODO HSS:  This needs to launch at least the following tasks:
        //
        // - If we decide to use separate disk-based key provisioning, a task to
        //   monitor our keys directory.
        // - If we own our identity key, a task to generate per-period sub-keys as
        //   needed.

        Ok(rend_req_rx)
    }

    /// Tell this onion service to stop running.
    ///
    /// It can be restarted with launch().
    ///
    /// You can also shut down an onion service completely by dropping the last
    /// Clone of it.
    pub fn stop(&self) {
        todo!() // TODO hss
    }
}

/// Generate the identity key of the service, unless it already exists or `offline_hsid` is `true`.
fn maybe_generate_hsid(
    keymgr: &Arc<KeyMgr>,
    nickname: &HsNickname,
    offline_hsid: bool,
) -> Result<(), StartupError> {
    let hsid_spec = HsIdKeypairSpecifier::new(nickname);
    let pub_hsid_spec = HsIdPublicKeySpecifier::new(nickname);

    let has_hsid_kp = keymgr
        .get::<HsIdKeypair>(&hsid_spec)
        .map_err(|cause| StartupError::Keystore {
            action: "read",
            cause,
        })?
        .is_some();

    let has_hsid_pub = keymgr
        .get::<HsIdKey>(&pub_hsid_spec)
        .map_err(|cause| StartupError::Keystore {
            action: "read",
            cause,
        })?
        .is_some();

    // If KS_hs_id is missing (and not stored offline), generate a new keypair.
    //
    // TODO HSS: if the hsid is missing but the service key directory exists, should we remove
    // any preexisting keys from it?
    if !offline_hsid {
        if !has_hsid_kp && has_hsid_pub {
            // The hsid keypair is missing, but the hsid public key is not, so we can't
            // generate a fresh keypair. We also cannot proceed, because the hsid is not
            // supposed to be offline
            warn!("offline_hsid is false, but KS_hs_id missing!");

            return Err(StartupError::KeystoreCorrupted);
        }

        // TODO HSS: make the selector configurable
        let keystore_sel = KeystoreSelector::Default;
        let mut rng = rand::thread_rng();

        // NOTE: KeyMgr::generate will generate a new hsid keypair and corresponding public
        // key.
        if keymgr
            .generate_with_derived::<HsIdKeypair, ed25519::PublicKey>(
                &hsid_spec,
                &pub_hsid_spec,
                keystore_sel,
                |sk| sk.public,
                &mut rng,
                false, /* overwrite */
            )
            .map_err(|cause| StartupError::Keystore {
                action: "generate key",
                cause,
            })?
            .is_some()
        {
            let kp = keymgr
                .get::<HsIdKeypair>(&hsid_spec)
                .map_err(|cause| StartupError::Keystore {
                    action: "read",
                    cause,
                })?
                .ok_or(StartupError::KeystoreCorrupted)?;

            let hsid: HsId = HsIdKey::from(&kp).into();
            info!(
                "Generated a new identity for service {nickname}: {}",
                sensitive(hsid)
            );
        } else {
            trace!("Using existing identity for service {nickname}");
        }
    }

    Ok(())
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

    use fs_mistrust::Mistrust;
    use tempfile::{tempdir, TempDir};

    use tor_basic_utils::test_rng::testing_rng;
    use tor_keymgr::ArtiNativeKeystore;
    use tor_llcrypto::util::rand_compat::RngCompatExt;

    use crate::{HsIdKeypairSpecifier, HsIdPublicKeySpecifier};

    /// The nickname of the test service.
    const TEST_SVC_NICKNAME: &str = "test-svc";

    fn create_keymgr(keystore_dir: &TempDir) -> Arc<KeyMgr> {
        let keystore = ArtiNativeKeystore::from_path_and_mistrust(
            keystore_dir,
            &Mistrust::new_dangerously_trust_everyone(),
        )
        .unwrap();

        Arc::new(KeyMgr::new(keystore, vec![]))
    }

    macro_rules! maybe_generate_hsid {
        ($keymgr:expr, $offline_hsid:expr) => {{
            let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
            let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
            let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

            assert!($keymgr.get::<HsIdKey>(&pub_hsid_spec).unwrap().is_none());
            assert!($keymgr.get::<HsIdKeypair>(&hsid_spec).unwrap().is_none());

            maybe_generate_hsid(&$keymgr, &nickname, $offline_hsid).unwrap();
        }};
    }

    /// Create a test hsid keypair.
    fn create_hsid() -> (HsIdKeypair, HsIdKey) {
        let mut rng = testing_rng().rng_compat();
        let keypair = ed25519::Keypair::generate(&mut rng);

        let id_pub = HsIdKey::from(keypair.public);
        let id_keypair = HsIdKeypair::from(ed25519::ExpandedKeypair {
            secret: ed25519::ExpandedSecretKey::from(&keypair.secret),
            public: id_pub.clone().into(),
        });

        (id_keypair, id_pub)
    }

    #[test]
    fn generate_hsid() {
        let keystore_dir = tempdir().unwrap();
        let keymgr = create_keymgr(&keystore_dir);

        let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
        let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
        let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

        maybe_generate_hsid!(keymgr, false /* offline_hsid */);

        let hsid_public = keymgr.get::<HsIdKey>(&pub_hsid_spec).unwrap().unwrap();
        let hsid_keypair = keymgr.get::<HsIdKeypair>(&hsid_spec).unwrap().unwrap();

        let keypair: ed25519::ExpandedKeypair = hsid_keypair.into();
        assert_eq!(hsid_public.as_ref(), &keypair.public);
    }

    #[test]
    fn hsid_keypair_already_exists() {
        let keystore_dir = tempdir().unwrap();
        let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
        let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
        let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

        for hsid_pub_missing in [false, true] {
            let keymgr = create_keymgr(&keystore_dir);

            // Insert the preexisting hsid keypair.
            let (existing_hsid_keypair, existing_hsid_public) = create_hsid();
            let existing_keypair: ed25519::ExpandedKeypair = existing_hsid_keypair.into();
            // Expanded keypairs are not clone, so we have to extract the private key bytes here to use
            // them in an assertion that comes after the insert()
            let existing_keypair_secret = existing_keypair.secret.to_bytes();

            let existing_hsid_keypair = HsIdKeypair::from(existing_keypair);

            keymgr
                .insert(existing_hsid_keypair, &hsid_spec, KeystoreSelector::Default)
                .unwrap();

            // Maybe the public key already exists too (in which case maybe_generate_hsid
            // doesn't need to insert it into the keystore).
            if hsid_pub_missing {
                keymgr
                    .insert(
                        existing_hsid_public.clone(),
                        &pub_hsid_spec,
                        KeystoreSelector::Default,
                    )
                    .unwrap();
            }

            maybe_generate_hsid(&keymgr, &nickname, false /* offline_hsid */).unwrap();

            let hsid_public = keymgr.get::<HsIdKey>(&pub_hsid_spec).unwrap().unwrap();
            let hsid_keypair = keymgr.get::<HsIdKeypair>(&hsid_spec).unwrap().unwrap();

            let keypair: ed25519::ExpandedKeypair = hsid_keypair.into();

            // The keypair was not overwritten. The public key matches the existing keypair.
            assert_eq!(hsid_public.as_ref(), existing_hsid_public.as_ref());
            assert_eq!(keypair.secret.to_bytes(), existing_keypair_secret);
        }
    }

    #[test]
    fn generate_hsid_offline_hsid() {
        let keystore_dir = tempdir().unwrap();
        let keymgr = create_keymgr(&keystore_dir);

        let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
        let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
        let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

        maybe_generate_hsid!(keymgr, true /* offline_hsid */);

        assert!(keymgr.get::<HsIdKey>(&pub_hsid_spec).unwrap().is_none());
        assert!(keymgr.get::<HsIdKeypair>(&hsid_spec).unwrap().is_none());
    }

    #[test]
    fn generate_hsid_missing_keypair() {
        let keystore_dir = tempdir().unwrap();
        let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
        let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
        let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

        let keymgr = create_keymgr(&keystore_dir);

        let (_hsid_keypair, hsid_public) = create_hsid();

        keymgr
            .insert(hsid_public, &pub_hsid_spec, KeystoreSelector::Default)
            .unwrap();

        // We're running with an online hsid, but the keypair is missing! The public part
        // of the key exists in the keystore, so we can't generate a new keypair.
        assert!(maybe_generate_hsid(&keymgr, &nickname, false /* offline_hsid */).is_err());
    }

    #[test]
    fn generate_hsid_corrupt_keystore() {
        let keystore_dir = tempdir().unwrap();
        let nickname = HsNickname::try_from(TEST_SVC_NICKNAME.to_string()).unwrap();
        let hsid_spec = HsIdKeypairSpecifier::new(&nickname);
        let pub_hsid_spec = HsIdPublicKeySpecifier::new(&nickname);

        let keymgr = create_keymgr(&keystore_dir);

        let (hsid_keypair, _hsid_public) = create_hsid();
        let (_hsid_keypair, hsid_public) = create_hsid();

        keymgr
            .insert(hsid_keypair, &hsid_spec, KeystoreSelector::Default)
            .unwrap();

        // Insert a mismatched public key
        keymgr
            .insert(hsid_public, &pub_hsid_spec, KeystoreSelector::Default)
            .unwrap();

        assert!(maybe_generate_hsid(&keymgr, &nickname, false /* offline_hsid */).is_err());
    }
}
