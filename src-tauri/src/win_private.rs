//! "Owned by this user, and private to it" on Windows: what a file's uid and
//! mode bits answer on Unix, asked of its owner SID and DACL.
//!
//! Until this existed both mailbox and object-quarantine checks answered
//! "yes" on Windows for anything, so a directory another user planted was
//! read and written as this user's own.
//!
//! What is trusted, and why:
//! - Owner: this process's `TokenUser` or `TokenOwner` SID, or Administrators.
//!   An elevated administrator's files are owned by Administrators (its
//!   `TokenOwner`), and a later unelevated run of the same user must still
//!   accept them; no other standard user can make Administrators the owner.
//! - Access: allow entries may name only those SIDs, SYSTEM, or OWNER RIGHTS.
//!   Like root on Unix, SYSTEM and Administrators can reach the file anyway.
//!   A missing DACL grants everyone everything, so it is not private.
//!
//! The parsing is plain functions over the bytes Windows returns, so it is
//! tested on every platform; only the calls that fetch and set those bytes
//! are Windows code.

/// `S-1-5-18`, LocalSystem.
#[cfg(any(windows, test))]
pub(crate) const SYSTEM_SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
/// `S-1-5-32-544`, BUILTIN\Administrators.
#[cfg(any(windows, test))]
pub(crate) const ADMINISTRATORS_SID: [u8; 16] =
    [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x20, 0x02, 0, 0];
/// `S-1-3-4`, OWNER RIGHTS: whoever the owner is, already checked.
#[cfg(any(windows, test))]
pub(crate) const OWNER_RIGHTS_SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0];

/// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_ALLOWED_CALLBACK_ACE_TYPE`: a SID
/// right after the mask.
#[cfg(any(windows, test))]
const PLAIN_ALLOW: [u8; 2] = [0, 9];
/// Allow entries whose SID sits behind object GUIDs or a compound header
/// (`ACCESS_ALLOWED_COMPOUND`, `_OBJECT`, `_CALLBACK_OBJECT`). Not read, so a
/// DACL holding one is not called private.
#[cfg(any(windows, test))]
const UNREAD_ALLOW: [u8; 3] = [4, 5, 11];
/// `INHERIT_ONLY_ACE`: the entry is for children, not this object.
#[cfg(any(windows, test))]
const INHERIT_ONLY: u8 = 0x08;
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`.
#[cfg(any(windows, test))]
const INHERIT_TO_CHILDREN: u8 = 0x03;
/// `FILE_ALL_ACCESS`.
#[cfg(any(windows, test))]
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

/// The SID at the start of `bytes`, by its own header: a revision byte of 1,
/// a sub-authority count of 1 to 15, a six-byte authority and that many
/// four-byte sub-authorities. `None` for anything else.
#[cfg(any(windows, test))]
pub(crate) fn sid_at(bytes: &[u8]) -> Option<&[u8]> {
    let header = bytes.get(..8)?;
    let count = usize::from(header[1]);
    if header[0] != 1 || count == 0 || count > 15 {
        return None;
    }
    bytes.get(..8 + 4 * count)
}

/// Whether `owner` is a SID this process may treat as its own.
#[cfg(any(windows, test))]
pub(crate) fn owner_trusted(owner: &[u8], mine: &[Vec<u8>]) -> bool {
    owner == ADMINISTRATORS_SID || mine.iter().any(|sid| sid.as_slice() == owner)
}

/// Whether the ACL in `acl` lets no one outside `mine`, Administrators,
/// SYSTEM and OWNER RIGHTS in. A malformed ACL is not private.
#[cfg(any(windows, test))]
pub(crate) fn acl_private(acl: &[u8], mine: &[Vec<u8>]) -> bool {
    let Some(header) = acl.get(..8) else {
        return false;
    };
    let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let count = usize::from(u16::from_le_bytes([header[4], header[5]]));
    if !matches!(header[0], 2 | 4) || size < 8 || size > acl.len() {
        return false;
    }
    let acl = &acl[..size];
    let trusted =
        |sid: &[u8]| sid == SYSTEM_SID || sid == OWNER_RIGHTS_SID || owner_trusted(sid, mine);
    let mut at = 8usize;
    for _ in 0..count {
        let Some(ace) = acl.get(at..at + 4) else {
            return false;
        };
        let (kind, flags) = (ace[0], ace[1]);
        let ace_size = usize::from(u16::from_le_bytes([ace[2], ace[3]]));
        let Some(body) = acl.get(at..at.saturating_add(ace_size)) else {
            return false;
        };
        if ace_size < 8 {
            return false;
        }
        at += ace_size;
        if flags & INHERIT_ONLY != 0 {
            continue;
        }
        if UNREAD_ALLOW.contains(&kind) {
            return false;
        }
        if !PLAIN_ALLOW.contains(&kind) {
            continue;
        }
        let mask = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        if mask == 0 {
            continue;
        }
        match sid_at(&body[8..]) {
            Some(sid) if trusted(sid) => {}
            _ => return false,
        }
    }
    true
}

/// A protected DACL giving each of `sids` full access, inherited by files and
/// directories made inside: what `0700` is on Unix. Built in `u32`s, because
/// an ACL must be DWORD-aligned in memory.
#[cfg(any(windows, test))]
pub(crate) fn private_acl(sids: &[&[u8]]) -> Vec<u32> {
    let mut bytes = vec![2u8, 0, 0, 0, 0, 0, 0, 0];
    for sid in sids {
        let ace_size = 8 + sid.len();
        bytes.extend_from_slice(&[0, INHERIT_TO_CHILDREN]);
        bytes.extend_from_slice(&u16::try_from(ace_size).unwrap_or(u16::MAX).to_le_bytes());
        bytes.extend_from_slice(&FILE_ALL_ACCESS.to_le_bytes());
        bytes.extend_from_slice(sid);
    }
    let size = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    bytes[2..4].copy_from_slice(&size.to_le_bytes());
    let count = u16::try_from(sids.len()).unwrap_or(u16::MAX);
    bytes[4..6].copy_from_slice(&count.to_le_bytes());
    bytes
        .chunks(4)
        .map(|word| {
            let mut padded = [0u8; 4];
            padded[..word.len()].copy_from_slice(word);
            u32::from_le_bytes(padded)
        })
        .collect()
}

#[cfg(windows)]
pub(crate) mod sys {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    /// `SE_FILE_OBJECT`.
    const FILE_OBJECT: u32 = 1;
    const OWNER_SECURITY_INFORMATION: u32 = 0x1;
    const DACL_SECURITY_INFORMATION: u32 = 0x4;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetSecurityInfo(
            handle: *mut c_void,
            object: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn GetNamedSecurityInfoW(
            name: *const u16,
            object: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn SetNamedSecurityInfoW(
            name: *const u16,
            object: u32,
            info: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
        fn GetLengthSid(sid: *mut c_void) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// An owner SID and DACL, copied out of the descriptor Windows returned.
    pub(crate) struct Security {
        pub owner: Vec<u8>,
        /// `None`: no DACL, which grants everyone everything.
        pub dacl: Option<Vec<u8>>,
    }

    impl Security {
        /// Owned by this process's user (or Administrators) and open to no one else.
        pub(crate) fn private_to_me(&self) -> bool {
            let mine = crate::sysload::winproc::sys::own_token_sids();
            super::owner_trusted(&self.owner, &mine)
                && self
                    .dacl
                    .as_deref()
                    .is_some_and(|dacl| super::acl_private(dacl, &mine))
        }

        /// Owned by this process's user (or Administrators).
        pub(crate) fn owned_by_me(&self) -> bool {
            let mine = crate::sysload::winproc::sys::own_token_sids();
            super::owner_trusted(&self.owner, &mine)
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Copies the owner and DACL out of a descriptor and frees it.
    ///
    /// # Safety
    /// `owner` and `dacl` point into `descriptor` (or are null), which a
    /// successful `Get*SecurityInfo` allocated with `LocalAlloc`.
    unsafe fn copy_out(
        owner: *mut c_void,
        dacl: *mut c_void,
        descriptor: *mut c_void,
    ) -> Option<Security> {
        let copied = (|| {
            if owner.is_null() {
                return None;
            }
            // SAFETY: `owner` is a valid SID inside the live descriptor.
            let owner_len = usize::try_from(unsafe { GetLengthSid(owner) }).ok()?;
            // SAFETY: GetLengthSid gave the SID's length, all inside the descriptor.
            let owner =
                unsafe { std::slice::from_raw_parts(owner.cast::<u8>(), owner_len) }.to_vec();
            let dacl = if dacl.is_null() {
                None
            } else {
                // SAFETY: an ACL starts with its eight-byte header; AclSize,
                // the u16 at offset 2, is the whole ACL's length.
                let header = unsafe { std::slice::from_raw_parts(dacl.cast::<u8>(), 8) };
                let size = usize::from(u16::from_le_bytes([header[2], header[3]])).max(8);
                // SAFETY: the ACL is `size` bytes inside the descriptor.
                Some(unsafe { std::slice::from_raw_parts(dacl.cast::<u8>(), size) }.to_vec())
            };
            Some(Security { owner, dacl })
        })();
        // SAFETY: `descriptor` was allocated by the successful call and is
        // freed exactly once, after the copies above.
        unsafe { LocalFree(descriptor) };
        copied
    }

    /// The owner and DACL of an open file or directory.
    pub(crate) fn of_handle(handle: std::os::windows::io::RawHandle) -> Option<Security> {
        let (mut owner, mut dacl, mut descriptor) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        // SAFETY: `handle` is a live file handle the caller holds open; every
        // out-pointer is writable, and the unrequested ones are null.
        let status = unsafe {
            GetSecurityInfo(
                handle.cast(),
                FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return None;
        }
        // SAFETY: the call succeeded, so the pointers come from `descriptor`.
        unsafe { copy_out(owner, dacl, descriptor) }
    }

    /// The owner and DACL of the file or directory at `path`.
    pub(crate) fn of_path(path: &Path) -> Option<Security> {
        let name = wide(path);
        let (mut owner, mut dacl, mut descriptor) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        // SAFETY: `name` is NUL-terminated; every out-pointer is writable,
        // and the unrequested ones are null.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return None;
        }
        // SAFETY: the call succeeded, so the pointers come from `descriptor`.
        unsafe { copy_out(owner, dacl, descriptor) }
    }

    /// Replaces the DACL of the directory at `path` with a protected one that
    /// gives this process's user (and its default owner) and SYSTEM full
    /// access, inherited by what is inside. Windows re-derives the inherited
    /// entries of existing children from it.
    pub(crate) fn make_private(path: &Path) -> Result<(), String> {
        let mine = crate::sysload::winproc::sys::own_token_sids();
        if mine.is_empty() {
            return Err("this process's own user could not be read".into());
        }
        let mut sids: Vec<&[u8]> = mine.iter().map(Vec::as_slice).collect();
        sids.push(&super::SYSTEM_SID);
        let mut acl = super::private_acl(&sids);
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and `acl` is a well-formed,
        // DWORD-aligned ACL that outlives the call; owner, group and SACL are
        // not set.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(
                std::io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(i32::MAX))
                    .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `S-1-5-21-1-2-3-1001`, a local user.
    const USER: [u8; 28] = [
        1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xE9, 3, 0, 0,
    ];
    /// `S-1-5-21-1-2-3-1002`, another one.
    const OTHER: [u8; 28] = [
        1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xEA, 3, 0, 0,
    ];
    /// `S-1-1-0`, Everyone.
    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    fn mine() -> Vec<Vec<u8>> {
        vec![USER.to_vec()]
    }

    /// One ACE of `kind` with `flags` and `mask` for `sid`.
    fn ace(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let mut out = vec![kind, flags];
        out.extend_from_slice(&u16::try_from(8 + sid.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&mask.to_le_bytes());
        out.extend_from_slice(sid);
        out
    }

    fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = aces.concat();
        let mut out = vec![2, 0];
        out.extend_from_slice(&u16::try_from(8 + body.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&u16::try_from(aces.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend(body);
        out
    }

    #[test]
    fn a_sid_is_read_by_its_own_header() {
        assert_eq!(sid_at(&USER), Some(&USER[..]));
        assert_eq!(sid_at(&SYSTEM_SID), Some(&SYSTEM_SID[..]));
        assert_eq!(
            sid_at(&[2, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]),
            None,
            "revision"
        );
        assert_eq!(sid_at(&[1, 0, 0, 0, 0, 0, 0, 5]), None, "no sub-authority");
        assert_eq!(sid_at(&USER[..20]), None, "cut short");
    }

    #[test]
    fn the_owner_is_this_user_or_administrators() {
        assert!(owner_trusted(&USER, &mine()));
        assert!(
            owner_trusted(&ADMINISTRATORS_SID, &mine()),
            "an elevated run's own"
        );
        assert!(!owner_trusted(&OTHER, &mine()));
        assert!(!owner_trusted(&SYSTEM_SID, &mine()));
    }

    /// A profile directory's usual DACL: the user, SYSTEM, Administrators.
    #[test]
    fn a_profile_style_dacl_is_private() {
        let dacl = acl(&[
            ace(0, 0x13, FILE_ALL_ACCESS, &USER),
            ace(0, 0x13, FILE_ALL_ACCESS, &SYSTEM_SID),
            ace(0, 0x13, FILE_ALL_ACCESS, &ADMINISTRATORS_SID),
        ]);
        assert!(acl_private(&dacl, &mine()));
    }

    #[test]
    fn any_grant_to_someone_else_is_not_private() {
        for stranger in [&OTHER[..], &EVERYONE[..]] {
            let dacl = acl(&[
                ace(0, 0, FILE_ALL_ACCESS, &USER),
                ace(0, 0, 0x0012_0089, stranger),
            ]);
            assert!(!acl_private(&dacl, &mine()));
        }
        let callback = acl(&[ace(9, 0, 1, &OTHER)]);
        assert!(
            !acl_private(&callback, &mine()),
            "a callback allow grants too"
        );
    }

    #[test]
    fn denials_inherit_only_entries_and_empty_masks_grant_nothing() {
        let dacl = acl(&[
            ace(0, 0, FILE_ALL_ACCESS, &USER),
            ace(1, 0, FILE_ALL_ACCESS, &EVERYONE),
            ace(0, INHERIT_ONLY | 0x03, FILE_ALL_ACCESS, &EVERYONE),
            ace(0, 0, 0, &OTHER),
        ]);
        assert!(acl_private(&dacl, &mine()));
    }

    #[test]
    fn an_allow_entry_that_is_not_read_is_not_private() {
        let mut object = ace(5, 0, 1, &USER);
        object.truncate(8);
        object.extend_from_slice(&[0; 4]);
        let size = u16::try_from(object.len()).unwrap();
        object[2..4].copy_from_slice(&size.to_le_bytes());
        assert!(!acl_private(&acl(&[object]), &mine()));
    }

    #[test]
    fn a_malformed_acl_is_not_private() {
        let good = acl(&[ace(0, 0, FILE_ALL_ACCESS, &USER)]);
        assert!(acl_private(&good, &mine()));
        assert!(!acl_private(&good[..good.len() - 1], &mine()), "cut short");
        let mut overcounted = good.clone();
        overcounted[4] = 2;
        assert!(
            !acl_private(&overcounted, &mine()),
            "more entries than bytes"
        );
        let mut tiny = good.clone();
        tiny[10] = 4;
        tiny[11] = 0;
        assert!(
            !acl_private(&tiny, &mine()),
            "an entry smaller than its header"
        );
        assert!(!acl_private(&[2, 0], &mine()));
        let mut revision = good;
        revision[0] = 9;
        assert!(!acl_private(&revision, &mine()));
    }

    /// The real calls, on a real directory: a new one is this user's; once
    /// made private it reads back private, and a file made inside inherits
    /// that, read through its open handle.
    #[cfg(windows)]
    #[test]
    fn a_directory_made_private_and_its_new_file_read_back_private() {
        use std::os::windows::io::AsRawHandle;
        let dir = tempfile::TempDir::new().unwrap();
        let before = sys::of_path(dir.path()).expect("a new directory's security reads");
        assert!(
            before.owned_by_me(),
            "a directory this process made is its own"
        );
        sys::make_private(dir.path()).expect("made private");
        let after = sys::of_path(dir.path()).expect("reads again");
        assert!(after.private_to_me());
        let file = std::fs::File::create(dir.path().join("inside")).unwrap();
        let inside = sys::of_handle(file.as_raw_handle()).expect("the file's security reads");
        assert!(
            inside.private_to_me(),
            "a file made inside inherits the private DACL"
        );
    }

    #[test]
    fn the_acl_made_private_is_read_back_as_private() {
        let made = bytes(&private_acl(&[&USER, &SYSTEM_SID]));
        assert!(acl_private(&made, &mine()));
        assert!(!acl_private(&made, &[OTHER.to_vec()]), "not someone else's");
        assert_eq!(made[0], 2, "ACL_REVISION");
        assert_eq!(
            usize::from(u16::from_le_bytes([made[2], made[3]])),
            made.len()
        );
        assert_eq!(u16::from_le_bytes([made[4], made[5]]), 2);
        assert_eq!(
            made[9], INHERIT_TO_CHILDREN,
            "files and folders inside inherit it"
        );
    }
}
