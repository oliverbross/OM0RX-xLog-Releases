#ifdef _WIN32
#include <windows.h>
#include <winhttp.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include "CClusterSocket.h"
#pragma comment(lib, "winhttp.lib")

// Local credentials are scoped to the installed Agent. Never follow redirects
// or allow a system proxy to receive this bearer token.
int xlog_local_agent_http(const char *path, const char *method, const char *authorization,
                          const void *body, int length, void *response, int capacity, int *size) {
    if (!path || !method || !authorization || !response || !size || length < 0 || capacity <= 0) return -1;
    if (strcmp(path,"/api/v1/state") && strcmp(path,"/api/v1/config") && strcmp(path,"/api/v1/commands")) return -1;
    if (strcmp(method,"GET") && strcmp(method,"PUT") && strcmp(method,"POST")) return -1;
    if (strlen(authorization)>4104 || strpbrk(authorization,"\r\n")) return -1;
    wchar_t route[64], verb[8], header[4200];
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,path,-1,route,64) ||
        !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,method,-1,verb,8)) return -1;
    wcscpy_s(header,4200,L"Authorization: ");
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,authorization,-1,header+15,4185)) return -1;
    wcscat_s(header,4200,L"\r\nContent-Type: application/json\r\n");
    HINTERNET session=WinHttpOpen(L"OM0RX-xLog",WINHTTP_ACCESS_TYPE_NO_PROXY,NULL,NULL,0), connection=NULL, request=NULL;
    int status=-1; *size=0;
    if (!session) goto done;
    if (!WinHttpSetTimeouts(session,2000,2000,4000,5000)) goto done;
    connection=WinHttpConnect(session,L"127.0.0.1",8840,0);
    if (!connection) goto done;
    request=WinHttpOpenRequest(connection,verb,route,NULL,WINHTTP_NO_REFERER,WINHTTP_DEFAULT_ACCEPT_TYPES,0);
    DWORD disable=WINHTTP_DISABLE_REDIRECTS|WINHTTP_DISABLE_COOKIES|WINHTTP_DISABLE_AUTHENTICATION;
    if (!request || !WinHttpSetOption(request,WINHTTP_OPTION_DISABLE_FEATURE,&disable,sizeof(disable))) goto done;
    if (!WinHttpSendRequest(request,header,(DWORD)-1,(void*)body,length,length,0) || !WinHttpReceiveResponse(request,NULL)) goto done;
    DWORD code=0, n=sizeof(code);
    if (!WinHttpQueryHeaders(request,WINHTTP_QUERY_STATUS_CODE|WINHTTP_QUERY_FLAG_NUMBER,NULL,&code,&n,NULL)) goto done;
    for (;;) {
        DWORD count=0; char chunk[8192];
        if (!WinHttpReadData(request,chunk,sizeof(chunk),&count)) goto done;
        if (count==0) break;
        if (count>(DWORD)(capacity-*size)) goto done;
        memcpy((char*)response+*size,chunk,count); *size+=(int)count;
    }
    status=(int)code;
done:
    SecureZeroMemory(header,sizeof(header));
    if(request) WinHttpCloseHandle(request);
    if(connection) WinHttpCloseHandle(connection);
    if(session) WinHttpCloseHandle(session);
    return status;
}
#endif
