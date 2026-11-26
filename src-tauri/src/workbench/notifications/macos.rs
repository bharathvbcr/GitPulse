//! Public UserNotifications APIs; no permissions are requested during setup.
use super::{
    Event, Status, WorkbenchError, RELIEF_PAUSE_ACTION, RELIEF_RESUME_ACTION,
    RELIEF_SETTINGS_ACTION, RELIEF_TERMINATE_ACTION,
};
use block2::{DynBlock, RcBlock};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    AnyThread, DefinedClass, MainThreadMarker,
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationAction, UNNotificationActionOptions, UNNotificationCategory,
    UNNotificationCategoryOptions, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::{
    cell::RefCell,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

struct DelegateData {
    sender: mpsc::SyncSender<Event>,
    status: Arc<Mutex<Status>>,
}
define_class!(
    // SAFETY: NSObject has no subclassing requirements. The callback signatures
    // match the generated UNUserNotificationCenterDelegate protocol bindings.
    #[unsafe(super = NSObject)]
    #[ivars = DelegateData]
    struct NotificationDelegate;
    unsafe impl NSObjectProtocol for NotificationDelegate {}
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let mut options =
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List;
            if notification.request().content().sound().is_some() {
                options |= UNNotificationPresentationOptions::Sound;
            }
            completion.call((options,));
        }
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn respond(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            let native = response.notification().request().identifier().to_string();
            // A pressure-relief banner is not an activity or session notice and
            // has no durable claim behind it. "Resume now" leaves the request
            // file the supervisor reads (`relief_status::request_resume`), here,
            // so it works in whichever launch macOS delivered it to; Settings
            // and the body click open Settings.
            let category = response
                .notification()
                .request()
                .content()
                .categoryIdentifier()
                .to_string();
            if matches!(
                category.as_str(),
                RELIEF_CATEGORY | RELIEF_OFFER_CATEGORY | RELIEF_ADVICE_CATEGORY
            ) {
                // Every request file is written off the delegate's thread
                // (the write is a sync and a rename, and AppKit is waiting on
                // this thread); the banner's own identifier says which tree.
                let action = response.actionIdentifier().to_string();
                let spawn = |name: &'static str,
                             what: &'static str,
                             body: Box<dyn FnOnce() + Send>| {
                    if let Err(error) = crate::supervisor::spawn_once(name, what, body) {
                        log::warn!(target:"workbench","{what} from a banner was not started: {error}");
                    }
                };
                match super::relief_tap(&category, &action, &native) {
                    super::ReliefTap::Resume => spawn(
                        "relief-resume",
                        "pressure relief resume request",
                        Box::new(|| {
                            if let Err(error) =
                                crate::sysload::relief_status::request_resume("notification")
                            {
                                log::warn!(target:"workbench","resume request from a banner was not written: {error}");
                            }
                        }),
                    ),
                    super::ReliefTap::Terminate(id) => spawn(
                        "relief-terminate",
                        "pressure relief end request",
                        Box::new(move || {
                            if let Err(error) =
                                crate::sysload::relief_status::request_terminate("notification", id)
                            {
                                log::warn!(target:"workbench","end request from a banner was not written: {error}");
                            }
                        }),
                    ),
                    super::ReliefTap::Pause(id) => spawn(
                        "relief-pause",
                        "pressure relief pause request",
                        Box::new(move || {
                            if let Err(error) =
                                crate::sysload::relief_status::request_pause("notification", id)
                            {
                                log::warn!(target:"workbench","pause request from a banner was not written: {error}");
                            }
                        }),
                    ),
                    super::ReliefTap::Settings => {
                        if let Err(error) = self.ivars().sender.try_send(Event::Relief) {
                            log::warn!(target:"workbench","relief banner activation could not be queued: {error}");
                        }
                    }
                    super::ReliefTap::Ignore => {
                        log::warn!(target:"workbench","a relief banner button named no tree and was ignored: {native}");
                    }
                }
                completion.call(());
                return;
            }
            // Snooze is the only action that is not an open: the body click
            // (UNNotificationDefaultActionIdentifier) and the Open button both
            // open. Dismiss never arrives — no category sets CustomDismissAction.
            let event = if response.actionIdentifier().to_string() == SNOOZE_ACTION {
                Event::Snooze(native)
            } else {
                Event::Activate(native)
            };
            if let Err(error) = self.ivars().sender.try_send(event) {
                log::warn!(target:"workbench","notification activation could not be queued: {error}");
                if let Ok(mut status) = self.ivars().status.lock() {
                    status.error = Some(
                        "Notification activation could not be queued. Open the activity inbox."
                            .into(),
                    );
                }
            }
            completion.call(());
        }
    }
);
thread_local! {static DELEGATE:RefCell<Option<Retained<NotificationDelegate>>>=const{RefCell::new(None)};}

fn center() -> Result<Retained<UNUserNotificationCenter>, WorkbenchError> {
    // Apple's center can raise an Objective-C exception for an unbundled
    // executable. Report unsupported instead of invoking it from CLI/tests.
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return Err(WorkbenchError::new(
            "unsupported",
            "Native notifications require a bundled GitPulse application.",
        ));
    }
    Ok(UNUserNotificationCenter::currentNotificationCenter())
}
pub(super) fn install(
    sender: mpsc::SyncSender<Event>,
    status: Arc<Mutex<Status>>,
) -> Result<(), WorkbenchError> {
    let _main = MainThreadMarker::new().ok_or_else(|| {
        WorkbenchError::new(
            "worker_error",
            "Notification delegate must initialize on the main thread.",
        )
    })?;
    let center = center()?;
    let allocated = NotificationDelegate::alloc().set_ivars(DelegateData { sender, status });
    // SAFETY: NSObject's init initializes this freshly allocated subclass.
    let delegate: Retained<NotificationDelegate> = unsafe { msg_send![super(allocated), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with(|slot| *slot.borrow_mut() = Some(delegate));
    // Two categories, because the two kinds of banner resolve to different
    // places when they are clicked: an activity notice opens the task it is
    // about, a session notice raises the terminal tab that asked. Registering
    // one category with a generic verb would make the button lie about one of
    // them. `setNotificationCategories` replaces the whole set, so both are
    // registered in the same call rather than in two.
    //
    // Only an activity notice can be snoozed: it is a durable inbox row with a
    // snooze field. A session banner has nothing behind it to defer. Snooze is
    // a background action (no Foreground option), so pressing it does not
    // bring GitPulse forward.
    let snooze_title = format!("Snooze {} hour", super::SNOOZE_SECONDS / 3600);
    let categories: Vec<_> = [
        (WORKBENCH_CATEGORY, "Open task", true),
        (SESSION_CATEGORY, "Open session", false),
    ]
    .into_iter()
    .map(|(identifier, verb, snoozes)| {
        let mut actions = vec![UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str("open"),
            &NSString::from_str(verb),
            UNNotificationActionOptions::Foreground,
        )];
        if snoozes {
            actions.push(UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(SNOOZE_ACTION),
                &NSString::from_str(&snooze_title),
                UNNotificationActionOptions::empty(),
            ));
        }
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(identifier),
            &NSArray::from_retained_slice(&actions),
            &NSArray::<NSString>::new(),
            UNNotificationCategoryOptions::empty(),
        )
    })
    .chain(
        // Resume, End it and Pause it are background actions: pressing one
        // must not bring GitPulse forward over the thing the person is trying
        // to get back to. A banner that offers a pause carries only Pause it
        // and Settings; advice about an untrusted repository carries Settings
        // alone, because there is no pause to offer.
        [
            (
                RELIEF_CATEGORY,
                vec![
                    (RELIEF_RESUME_ACTION, "Resume now", false),
                    (RELIEF_TERMINATE_ACTION, "End it", false),
                    (RELIEF_SETTINGS_ACTION, "Settings", true),
                ],
            ),
            (
                RELIEF_OFFER_CATEGORY,
                vec![
                    (RELIEF_PAUSE_ACTION, "Pause it", false),
                    (RELIEF_SETTINGS_ACTION, "Settings", true),
                ],
            ),
            (
                RELIEF_ADVICE_CATEGORY,
                vec![(RELIEF_SETTINGS_ACTION, "Settings", true)],
            ),
        ]
        .into_iter()
        .map(|(category, buttons)| {
            let actions: Vec<_> = buttons
                .into_iter()
                .map(|(identifier, title, foreground)| {
                    UNNotificationAction::actionWithIdentifier_title_options(
                        &NSString::from_str(identifier),
                        &NSString::from_str(title),
                        if foreground {
                            UNNotificationActionOptions::Foreground
                        } else {
                            UNNotificationActionOptions::empty()
                        },
                    )
                })
                .collect();
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(category),
                &NSArray::from_retained_slice(&actions),
                &NSArray::<NSString>::new(),
                UNNotificationCategoryOptions::empty(),
            )
        }),
    )
    .collect();
    center.setNotificationCategories(&NSSet::from_retained_slice(&categories));
    Ok(())
}

/// Category and thread for the workbench's activity notices.
pub(super) const WORKBENCH_CATEGORY: &str = "gitpulse-workbench";
/// Category and thread for terminal and agent session notices. A separate
/// thread so macOS groups a noisy session apart from task activity instead of
/// collapsing both into one stack.
pub(super) const SESSION_CATEGORY: &str = "gitpulse-session";
/// Action identifier of an activity banner's Snooze button.
const SNOOZE_ACTION: &str = "snooze";
/// Category and thread of the pressure-relief supervisor's banners
/// (`desktop::pressure`).
pub(super) const RELIEF_CATEGORY: &str = "gitpulse-relief";
/// The category of a banner that offers to pause a tree, and of one that only
/// advises. The buttons' identifiers are `notifications`' (`RELIEF_*_ACTION`).
pub(super) const RELIEF_OFFER_CATEGORY: &str = "gitpulse-relief-offer";
pub(super) const RELIEF_ADVICE_CATEGORY: &str = "gitpulse-relief-advice";

fn timeout() -> WorkbenchError {
    WorkbenchError::new("timeout","macOS notification reply was not confirmed. Recheck status; a delivery will not be sent again automatically.")
}
pub(super) fn authorization(request: bool) -> Result<String, WorkbenchError> {
    let center = center()?;
    if request {
        let (sender, receiver) = mpsc::sync_channel(1);
        let callback = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            let _ = sender.try_send((granted.as_bool(), error.is_null()));
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &callback,
        );
        let (_granted, valid) = receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| timeout())?;
        if !valid {
            return Err(WorkbenchError::new(
                "os_error",
                "macOS could not complete the notification authorization request.",
            ));
        }
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: Apple's callback supplies a live settings object during this
        // invocation. Read the value here; never retain/send a borrowed pointer.
        let state = unsafe { settings.as_ref() }.authorizationStatus();
        let state = match state {
            UNAuthorizationStatus::Authorized => "authorized",
            UNAuthorizationStatus::Provisional => "provisional",
            UNAuthorizationStatus::Denied => "denied",
            UNAuthorizationStatus::NotDetermined => "not_determined",
            _ => "unknown",
        };
        let _ = sender.try_send(state.to_owned());
    });
    center.getNotificationSettingsWithCompletionHandler(&callback);
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| timeout())
}

pub(super) fn submit(native: &str, title: &str, sound: bool) -> Result<bool, WorkbenchError> {
    post(
        native,
        super::ACTIVITY_HEADING,
        title,
        sound,
        WORKBENCH_CATEGORY,
    )
}

/// One submission, whatever raised it.
///
/// `identifier` is what macOS replaces on: re-posting the same one updates the
/// existing banner rather than stacking another, which is what makes a chatty
/// session produce one live notification instead of a column of them.
pub(super) fn post(
    native: &str,
    heading: &str,
    body: &str,
    sound: bool,
    category: &str,
) -> Result<bool, WorkbenchError> {
    if native.is_empty() {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Notification identifier cannot be empty.",
        ));
    }
    let center = center()?;
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(heading));
    content.setBody(&NSString::from_str(body));
    content.setCategoryIdentifier(&NSString::from_str(category));
    content.setThreadIdentifier(&NSString::from_str(category));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(native),
        &content,
        None,
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |error: *mut NSError| {
        let _ = sender.try_send(error.is_null());
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| timeout())
}

/// Identifiers of the activity banners Notification Center still shows.
/// Session banners are filtered out here by category; the coordinator then
/// refuses anything whose identity is not this profile's saved claim.
pub(super) fn delivered() -> Result<Vec<String>, WorkbenchError> {
    let center = center()?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |list: std::ptr::NonNull<NSArray<UNNotification>>| {
        // SAFETY: Apple's callback supplies a live array during this
        // invocation. Copy the identifiers out; never retain the borrow.
        let list = unsafe { list.as_ref() };
        let identifiers: Vec<String> = list
            .iter()
            .filter(|n| {
                n.request().content().categoryIdentifier().to_string() == WORKBENCH_CATEGORY
            })
            .map(|n| n.request().identifier().to_string())
            .collect();
        let _ = sender.try_send(identifiers);
    });
    center.getDeliveredNotificationsWithCompletionHandler(&callback);
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| timeout())
}

/// Take banners down. Fire-and-forget by Apple's design (no completion
/// handler); an identifier no longer delivered is ignored by the center.
pub(super) fn withdraw(natives: &[String]) {
    let Ok(center) = center() else { return };
    let identifiers: Vec<_> = natives.iter().map(|n| NSString::from_str(n)).collect();
    center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&identifiers));
}

pub(super) fn local_minute() -> Result<u16, WorkbenchError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| {
            WorkbenchError::new("clock_error", "Local notification clock is unavailable.")
        })?;
    let seconds = libc::time_t::try_from(duration.as_secs()).map_err(|_| {
        WorkbenchError::new(
            "clock_error",
            "Local notification clock exceeds the supported range.",
        )
    })?;
    let mut result = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid for the call. Read only after localtime_r
    // returns the initialized output pointer; libc applies local timezone/DST.
    let valid = unsafe { !libc::localtime_r(&seconds, result.as_mut_ptr()).is_null() };
    if !valid {
        return Err(WorkbenchError::new(
            "clock_error",
            "Cannot resolve local quiet hours.",
        ));
    }
    let time = unsafe { result.assume_init() };
    if !(0..24).contains(&time.tm_hour) || !(0..60).contains(&time.tm_min) {
        return Err(WorkbenchError::new(
            "clock_error",
            "Invalid local calendar time.",
        ));
    }
    u16::try_from(time.tm_hour * 60 + time.tm_min)
        .map_err(|_| WorkbenchError::new("clock_error", "Invalid local calendar minute."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbundled_center_and_notification_calls_fail_closed_with_unsupported() {
        assert_eq!(
            post("", "Claude", "Needs input", false, SESSION_CATEGORY)
                .unwrap_err()
                .code,
            "invalid_input"
        );
        if NSBundle::mainBundle().bundleIdentifier().is_some() {
            return;
        }
        assert_eq!(center().unwrap_err().code, "unsupported");
        assert_eq!(authorization(false).unwrap_err().code, "unsupported");
        assert_eq!(authorization(true).unwrap_err().code, "unsupported");
        assert_eq!(
            submit("gitpulse.test.event-1", "Title", false)
                .unwrap_err()
                .code,
            "unsupported"
        );
        assert_eq!(
            post(
                "gitpulse.session.t1",
                "Claude",
                "Needs input",
                true,
                SESSION_CATEGORY,
            )
            .unwrap_err()
            .code,
            "unsupported"
        );
        assert_eq!(delivered().unwrap_err().code, "unsupported");

        // Off-main-thread install must fail closed with worker_error regardless
        // of whether the test runner executes tests on the main thread.
        let install_err = std::thread::spawn(|| {
            let (tx, _rx) = mpsc::sync_channel(1);
            let status = Arc::new(Mutex::new(Status::default()));
            install(tx, status)
        })
        .join()
        .expect("join install thread")
        .unwrap_err();
        assert_eq!(install_err.code, "worker_error");

        // Safe no-op when unbundled.
        withdraw(&["gitpulse.test.event-1".to_owned()]);
    }

    #[test]
    fn notification_categories_and_local_minute_are_well_formed() {
        assert_eq!(WORKBENCH_CATEGORY, "gitpulse-workbench");
        assert_eq!(SESSION_CATEGORY, "gitpulse-session");
        assert_eq!(SNOOZE_ACTION, "snooze");
        assert_ne!(WORKBENCH_CATEGORY, SESSION_CATEGORY);
        assert_eq!(RELIEF_CATEGORY, "gitpulse-relief");
        assert_ne!(RELIEF_RESUME_ACTION, RELIEF_SETTINGS_ACTION);
        assert_ne!(RELIEF_CATEGORY, SESSION_CATEGORY);
        assert_ne!(RELIEF_CATEGORY, WORKBENCH_CATEGORY);

        let minute = local_minute().expect("local_minute succeeds");
        assert!(minute < 1440, "minute out of range: {minute}");
    }
}
