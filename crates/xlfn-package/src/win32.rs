#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code,
    unreachable_pub,
    clippy::all,
    reason = "Generated code from windows-bindgen"
)]

windows_link::link!("kernel32.dll" "system" fn CloseHandle(hobject : HANDLE) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn ConvertStringSecurityDescriptorToSecurityDescriptorW(stringsecuritydescriptor : PCWSTR, stringsdrevision : u32, securitydescriptor : *mut PSECURITY_DESCRIPTOR, securitydescriptorsize : *mut u32) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn ConvertStringSidToSidW(stringsid : PCWSTR, sid : *mut PSID) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn CreateDirectoryW(lppathname : PCWSTR, lpsecurityattributes : *const SECURITY_ATTRIBUTES) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn EqualSid(psid1 : PSID, psid2 : PSID) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn GetAce(pacl : *const ACL, dwaceindex : u32, pace : *mut *mut core::ffi::c_void) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn GetAclInformation(pacl : *const ACL, paclinformation : *mut core::ffi::c_void, naclinformationlength : u32, dwaclinformationclass : ACL_INFORMATION_CLASS) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess() -> HANDLE);
windows_link::link!("kernel32.dll" "system" fn GetFileInformationByHandle(hfile : HANDLE, lpfileinformation : *mut BY_HANDLE_FILE_INFORMATION) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn GetFileInformationByHandleEx(hfile : HANDLE, fileinformationclass : FILE_INFO_BY_HANDLE_CLASS, lpfileinformation : *mut core::ffi::c_void, dwbuffersize : u32) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn GetLengthSid(psid : PSID) -> u32);
windows_link::link!("advapi32.dll" "system" fn GetNamedSecurityInfoW(pobjectname : PCWSTR, objecttype : SE_OBJECT_TYPE, securityinfo : SECURITY_INFORMATION, ppsidowner : *mut PSID, ppsidgroup : *mut PSID, ppdacl : *mut PACL, ppsacl : *mut PACL, ppsecuritydescriptor : *mut PSECURITY_DESCRIPTOR) -> u32);
windows_link::link!("advapi32.dll" "system" fn GetSecurityDescriptorControl(psecuritydescriptor : PSECURITY_DESCRIPTOR, pcontrol : *mut u16, lpdwrevision : *mut u32) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn GetSecurityDescriptorDacl(psecuritydescriptor : PSECURITY_DESCRIPTOR, lpbdaclpresent : *mut BOOL, pdacl : *mut PACL, lpbdacldefaulted : *mut BOOL) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn GetTokenInformation(tokenhandle : HANDLE, tokeninformationclass : TOKEN_INFORMATION_CLASS, tokeninformation : *mut core::ffi::c_void, tokeninformationlength : u32, returnlength : *mut u32) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn IsValidSid(psid : PSID) -> BOOL);
windows_link::link!("kernel32.dll" "system" fn LocalFree(hmem : HLOCAL) -> HLOCAL);
windows_link::link!("kernel32.dll" "system" fn MoveFileExW(lpexistingfilename : PCWSTR, lpnewfilename : PCWSTR, dwflags : u32) -> BOOL);
windows_link::link!("advapi32.dll" "system" fn OpenProcessToken(processhandle : HANDLE, desiredaccess : u32, tokenhandle : *mut HANDLE) -> BOOL);
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ACCESS_ALLOWED_ACE {
    pub Header: ACE_HEADER,
    pub Mask: ACCESS_MASK,
    pub SidStart: u32,
}
pub type ACCESS_MASK = u32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ACE_HEADER {
    pub AceType: u8,
    pub AceFlags: u8,
    pub AceSize: u16,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ACL {
    pub AclRevision: u8,
    pub Sbz1: u8,
    pub AclSize: u16,
    pub AceCount: u16,
    pub Sbz2: u16,
}
pub type ACL_INFORMATION_CLASS = i32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ACL_SIZE_INFORMATION {
    pub AceCount: u32,
    pub AclBytesInUse: u32,
    pub AclBytesFree: u32,
}
pub const AclSizeInformation: ACL_INFORMATION_CLASS = 2;
pub type BOOL = i32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BY_HANDLE_FILE_INFORMATION {
    pub dwFileAttributes: u32,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub dwVolumeSerialNumber: u32,
    pub nFileSizeHigh: u32,
    pub nFileSizeLow: u32,
    pub nNumberOfLinks: u32,
    pub nFileIndexHigh: u32,
    pub nFileIndexLow: u32,
}
pub const DACL_SECURITY_INFORMATION: i32 = 4;
pub const ERROR_SHARING_VIOLATION: i32 = 32;
pub const ERROR_SUCCESS: i32 = 0;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}
pub const FILE_FLAG_BACKUP_SEMANTICS: i32 = 33554432;
pub const FILE_FLAG_OPEN_REPARSE_POINT: i32 = 2097152;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FILE_ID_128 {
    pub Identifier: [u8; 16],
}
impl Default for FILE_ID_128 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILE_ID_INFO {
    pub VolumeSerialNumber: u64,
    pub FileId: FILE_ID_128,
}
pub type FILE_INFO_BY_HANDLE_CLASS = i32;
pub const FILE_SHARE_DELETE: i32 = 4;
pub const FILE_SHARE_READ: i32 = 1;
pub const FILE_SHARE_WRITE: i32 = 2;
pub const FileAlignmentInfo: FILE_INFO_BY_HANDLE_CLASS = 17;
pub const FileAllocationInfo: FILE_INFO_BY_HANDLE_CLASS = 5;
pub const FileAttributeTagInfo: FILE_INFO_BY_HANDLE_CLASS = 9;
pub const FileBasicInfo: FILE_INFO_BY_HANDLE_CLASS = 0;
pub const FileCaseSensitiveInfo: FILE_INFO_BY_HANDLE_CLASS = 23;
pub const FileCompressionInfo: FILE_INFO_BY_HANDLE_CLASS = 8;
pub const FileDispositionInfo: FILE_INFO_BY_HANDLE_CLASS = 4;
pub const FileDispositionInfoEx: FILE_INFO_BY_HANDLE_CLASS = 21;
pub const FileEndOfFileInfo: FILE_INFO_BY_HANDLE_CLASS = 6;
pub const FileFullDirectoryInfo: FILE_INFO_BY_HANDLE_CLASS = 14;
pub const FileFullDirectoryRestartInfo: FILE_INFO_BY_HANDLE_CLASS = 15;
pub const FileIdBothDirectoryInfo: FILE_INFO_BY_HANDLE_CLASS = 10;
pub const FileIdBothDirectoryRestartInfo: FILE_INFO_BY_HANDLE_CLASS = 11;
pub const FileIdExtdDirectoryInfo: FILE_INFO_BY_HANDLE_CLASS = 19;
pub const FileIdExtdDirectoryRestartInfo: FILE_INFO_BY_HANDLE_CLASS = 20;
pub const FileIdInfo: FILE_INFO_BY_HANDLE_CLASS = 18;
pub const FileIoPriorityHintInfo: FILE_INFO_BY_HANDLE_CLASS = 12;
pub const FileNameInfo: FILE_INFO_BY_HANDLE_CLASS = 2;
pub const FileNormalizedNameInfo: FILE_INFO_BY_HANDLE_CLASS = 24;
pub const FileRemoteProtocolInfo: FILE_INFO_BY_HANDLE_CLASS = 13;
pub const FileRenameInfo: FILE_INFO_BY_HANDLE_CLASS = 3;
pub const FileRenameInfoEx: FILE_INFO_BY_HANDLE_CLASS = 22;
pub const FileStandardInfo: FILE_INFO_BY_HANDLE_CLASS = 1;
pub const FileStorageInfo: FILE_INFO_BY_HANDLE_CLASS = 16;
pub const FileStreamInfo: FILE_INFO_BY_HANDLE_CLASS = 7;
pub type HANDLE = *mut core::ffi::c_void;
pub type HLOCAL = HANDLE;
pub const MOVEFILE_REPLACE_EXISTING: i32 = 1;
pub const MOVEFILE_WRITE_THROUGH: i32 = 8;
pub const MaximumFileInfoByHandleClass: FILE_INFO_BY_HANDLE_CLASS = 25;
pub const OWNER_SECURITY_INFORMATION: i32 = 1;
pub type PACL = *mut ACL;
pub type PCWSTR = *const u16;
pub type PSECURITY_DESCRIPTOR = *mut core::ffi::c_void;
pub type PSID = *mut core::ffi::c_void;
pub const SDDL_REVISION_1: i32 = 1;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SECURITY_ATTRIBUTES {
    pub nLength: u32,
    pub lpSecurityDescriptor: *mut core::ffi::c_void,
    pub bInheritHandle: BOOL,
}
pub type SECURITY_DESCRIPTOR_CONTROL = u16;
pub type SECURITY_INFORMATION = u32;
pub const SE_DACL_PROTECTED: i32 = 4096;
pub const SE_FILE_OBJECT: SE_OBJECT_TYPE = 1;
pub type SE_OBJECT_TYPE = i32;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SID_AND_ATTRIBUTES {
    pub Sid: PSID,
    pub Attributes: u32,
}
pub type TOKEN_INFORMATION_CLASS = i32;
pub const TOKEN_QUERY: i32 = 8;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TOKEN_USER {
    pub User: SID_AND_ATTRIBUTES,
}
pub const TokenUser: TOKEN_INFORMATION_CLASS = 1;
