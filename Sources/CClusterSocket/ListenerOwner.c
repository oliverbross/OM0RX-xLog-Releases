#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <iphlpapi.h>
#include <stdlib.h>
#include <wchar.h>
#include "CClusterSocket.h"
#pragma comment(lib, "iphlpapi.lib")

static int xlog_smartsdr_cat_image(DWORD pid) {
    HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (!process) return 0;
    wchar_t path[MAX_PATH * 2];
    DWORD length = (DWORD)(sizeof(path) / sizeof(path[0]));
    int matched = 0;
    if (QueryFullProcessImageNameW(process, 0, path, &length) && length > 0) {
        const wchar_t *name = wcsrchr(path, L'\\');
        name = name ? name + 1 : path;
        // FlexRadio installs it as ...\SmartSDR v<version>\CAT\Cat.exe.
        if (_wcsicmp(name, L"Cat.exe") == 0) {
            for (wchar_t *c = path; *c; c++) *c = (wchar_t)towlower(*c);
            matched = wcsstr(path, L"\\smartsdr") != NULL;
        }
    }
    CloseHandle(process);
    return matched;
}

int xlog_tcp_listener_is_smartsdr_cat(uint16_t port) {
    ULONG size = 0;
    if (GetExtendedTcpTable(NULL, &size, FALSE, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0) != ERROR_INSUFFICIENT_BUFFER || size == 0) return 0;
    size += 4096;
    MIB_TCPTABLE_OWNER_PID *table = (MIB_TCPTABLE_OWNER_PID *)malloc(size);
    if (!table) return 0;
    int found = 0, other = 0;
    if (GetExtendedTcpTable(table, &size, FALSE, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0) == NO_ERROR) {
        for (DWORD i = 0; i < table->dwNumEntries; i++) {
            MIB_TCPROW_OWNER_PID *row = &table->table[i];
            if (ntohs((u_short)row->dwLocalPort) != port) continue;
            // Only listeners reachable on loopback matter: 127.0.0.1 or 0.0.0.0.
            u_long address = ntohl(row->dwLocalAddr);
            if (address != INADDR_ANY && (address >> 24) != 127) continue;
            if (xlog_smartsdr_cat_image(row->dwOwningPid)) found = 1; else other = 1;
        }
    }
    free(table);
    // Any other reachable owner on the same port keeps the port treated as a companion.
    return found && !other;
}
#endif
