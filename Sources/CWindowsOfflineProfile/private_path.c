#include "include/CWindowsOfflineProfile.h"
#include <windows.h>
#include <aclapi.h>
#include <sddl.h>

int xlog_create_private_windows_directory(const uint16_t *path) {
    PSECURITY_DESCRIPTOR descriptor = NULL;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
        L"D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
        SDDL_REVISION_1, &descriptor, NULL)) return 0;
    SECURITY_ATTRIBUTES attributes = { sizeof(attributes), descriptor, FALSE };
    BOOL created = CreateDirectoryW((LPCWSTR)path, &attributes);
    LocalFree(descriptor);
    return created != FALSE;
}

// A marked Windows fixture is private to its owner, Administrators and SYSTEM.
// Reject links and unknown grant types. The same check runs before WebView2
// starts and before the worker opens a disposable logbook.
static BOOL private_acl(PSID user, PSID owner, PACL acl) {
    if (!owner || !acl || !IsValidAcl(acl) ||
        !(EqualSid(user, owner) || IsWellKnownSid(owner, WinBuiltinAdministratorsSid))) return FALSE;
    for (DWORD i = 0; i < acl->AceCount; ++i) {
        ACE_HEADER *header;
        if (!GetAce(acl, i, (void **)&header)) return FALSE;
        if (header->AceType == ACCESS_DENIED_ACE_TYPE) continue;
        if (header->AceType != ACCESS_ALLOWED_ACE_TYPE) return FALSE;
        PSID principal = &((ACCESS_ALLOWED_ACE *)header)->SidStart;
        if (EqualSid(user, principal) || IsWellKnownSid(principal, WinCreatorOwnerRightsSid) ||
            IsWellKnownSid(principal, WinLocalSystemSid) ||
            IsWellKnownSid(principal, WinBuiltinAdministratorsSid)) continue;
        if ((header->AceFlags & INHERIT_ONLY_ACE) && IsWellKnownSid(principal, WinCreatorOwnerSid)) continue;
        return FALSE;
    }
    return TRUE;
}

int xlog_private_windows_path(const uint16_t *path, int directory) {
    HANDLE file = CreateFileW((LPCWSTR)path, READ_CONTROL | FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
    if (file == INVALID_HANDLE_VALUE) return 0;
    BY_HANDLE_FILE_INFORMATION info;
    HANDLE token = NULL;
    TOKEN_USER *user = NULL;
    PSECURITY_DESCRIPTOR descriptor = NULL;
    PSID owner = NULL;
    PACL acl = NULL;
    DWORD size = 0;
    BOOL accepted = FALSE;
    if (!GetFileInformationByHandle(file, &info) || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT ||
        !!(info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != !!directory ||
        (!directory && info.nNumberOfLinks != 1)) goto done;
    if (GetSecurityInfo(file, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        &owner, NULL, &acl, NULL, &descriptor) != ERROR_SUCCESS) goto done;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) goto done;
    GetTokenInformation(token, TokenUser, NULL, 0, &size);
    if (!size || !(user = LocalAlloc(LPTR, size))) goto done;
    if (GetTokenInformation(token, TokenUser, user, size, &size)) accepted = private_acl(user->User.Sid, owner, acl);
done:
    if (user) LocalFree(user);
    if (token) CloseHandle(token);
    if (descriptor) LocalFree(descriptor);
    CloseHandle(file);
    return accepted != FALSE;
}
