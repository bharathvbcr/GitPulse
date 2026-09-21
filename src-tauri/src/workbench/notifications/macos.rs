//! Public UserNotifications APIs; no permissions are requested during setup.
use super::{Event, Status, WorkbenchError};
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
            if let Err(error) = self.ivars().sender.try_send(Event::Activate(native)) {
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
    let categories: Vec<_> = [
        (WORKBENCH_CATEGORY, "Open task"),
        (SESSION_CATEGORY, "Open session"),
    ]
    .into_iter()
    .map(|(identifier, verb)| {
        let action = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str("open"),
            &NSString::from_str(verb),
            UNNotificationActionOptions::Foreground,
        );
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(identifier),
            &NSArray::from_retained_slice(&[action]),
            &NSArray::<NSString>::new(),
            UNNotificationCategoryOptions::empty(),
        )
    })
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
        "GitPulse activity",
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
