#include "CClusterSocket.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
typedef SOCKET socket_handle;
#define BAD_SOCKET INVALID_SOCKET
#define close_socket closesocket
#define socket_errno() WSAGetLastError()
#define socket_pending(e) ((e) == WSAEWOULDBLOCK || (e) == WSAEINPROGRESS)
#else
#include <sys/socket.h>
#include <netdb.h>
#include <unistd.h>
#include <fcntl.h>
#include <poll.h>
#include <errno.h>
#include <time.h>
typedef int socket_handle;
#define BAD_SOCKET (-1)
#define close_socket close
#define socket_errno() errno
#define socket_pending(e) ((e) == EAGAIN || (e) == EWOULDBLOCK || (e) == EINPROGRESS || (e) == EINTR)
#endif

struct XLogSocket { socket_handle fd; };
struct XLogSocketAddresses { struct addrinfo *values; };
struct XLogSocketCancellation { atomic_int cancelled; };

XLogSocketCancellation *xlog_socket_cancellation_create(void) {
    XLogSocketCancellation *token = malloc(sizeof(*token));
    if (token) atomic_init(&token->cancelled, 0);
    return token;
}
void xlog_socket_cancel(XLogSocketCancellation *token) { if (token) atomic_store(&token->cancelled, 1); }
int xlog_socket_cancelled(const XLogSocketCancellation *token) { return token && atomic_load(&token->cancelled); }
void xlog_socket_cancellation_destroy(XLogSocketCancellation *token) { free(token); }

static uint64_t milliseconds(void) {
#ifdef _WIN32
    return GetTickCount64();
#else
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
#endif
}

static int nonblocking(socket_handle fd) {
#ifdef _WIN32
    u_long enabled = 1;
    return ioctlsocket(fd, FIONBIO, &enabled) == 0;
#else
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0) return 0;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) < 0) return 0;
#ifdef SO_NOSIGPIPE
    int enabled = 1;
    if (setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &enabled, sizeof(enabled)) < 0) return 0;
#endif
    return 1;
#endif
}

static int wait_socket(socket_handle fd, int writing, int timeout_ms) {
#ifdef _WIN32
    WSAPOLLFD item = {fd, writing ? POLLWRNORM : POLLRDNORM, 0};
    int result = WSAPoll(&item, 1, timeout_ms);
#else
    struct pollfd item = {fd, writing ? POLLOUT : POLLIN, 0};
    int result = poll(&item, 1, timeout_ms);
    if (result < 0 && errno == EINTR) return 0;
#endif
    if (result <= 0) return result;
    if (item.revents & POLLNVAL) return -1;
    // HUP may still have buffered data. Let recv/SO_ERROR determine the result.
    return 1;
}

XLogSocketAddresses *xlog_socket_resolve(const char *host, uint16_t port) {
    if (!host || !*host || !port) return NULL;
#ifdef _WIN32
    WSADATA winsock;
    if (WSAStartup(MAKEWORD(2, 2), &winsock) != 0) return NULL;
#endif
    struct addrinfo hints, *addresses = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_protocol = IPPROTO_TCP;
    char service[6];
    snprintf(service, sizeof(service), "%u", port);
    if (getaddrinfo(host, service, &hints, &addresses) == 0) {
        XLogSocketAddresses *result = malloc(sizeof(*result));
        if (result) { result->values = addresses; return result; }
        freeaddrinfo(addresses);
    }
#ifdef _WIN32
    WSACleanup();
#endif
    return NULL;
}

void xlog_socket_addresses_destroy(XLogSocketAddresses *addresses) {
    if (!addresses) return;
    freeaddrinfo(addresses->values);
    free(addresses);
#ifdef _WIN32
    WSACleanup();
#endif
}

XLogSocket *xlog_socket_connect_addresses(const XLogSocketAddresses *addresses,
                                         int timeout_ms, const XLogSocketCancellation *token) {
    if (!addresses || timeout_ms <= 0 || xlog_socket_cancelled(token)) return NULL;
#ifdef _WIN32
    // A connected socket outlives its address list and owns a separate runtime
    // reference, balanced by socket_close even after resolver cleanup.
    WSADATA winsock;
    if (WSAStartup(MAKEWORD(2, 2), &winsock) != 0) return NULL;
#endif
    socket_handle connected = BAD_SOCKET;
    uint64_t deadline = milliseconds() + (uint64_t)timeout_ms;
    for (struct addrinfo *address = addresses->values; address && !xlog_socket_cancelled(token); address = address->ai_next) {
        if (milliseconds() >= deadline) break;
        socket_handle fd = socket(address->ai_family, address->ai_socktype, address->ai_protocol);
        if (fd == BAD_SOCKET) continue;
        if (!nonblocking(fd)) { close_socket(fd); continue; }
        int result = connect(fd, address->ai_addr, (int)address->ai_addrlen);
        int ready = result == 0;
        if (!ready && socket_pending(socket_errno())) {
            while (!xlog_socket_cancelled(token)) {
                uint64_t now = milliseconds();
                if (now >= deadline) break;
                int remaining = (int)(deadline - now);
                int waited = wait_socket(fd, 1, remaining < 100 ? remaining : 100);
                if (waited < 0) break;
                if (waited == 0) continue;
                int error = 0;
#ifdef _WIN32
                int length = sizeof(error);
#else
                socklen_t length = sizeof(error);
#endif
                ready = getsockopt(fd, SOL_SOCKET, SO_ERROR, (void *)&error, &length) == 0 && error == 0;
                break;
            }
        }
        if (ready && !xlog_socket_cancelled(token)) { connected = fd; break; }
        close_socket(fd);
    }
    if (connected != BAD_SOCKET) {
        XLogSocket *result = malloc(sizeof(*result));
        if (result) { result->fd = connected; return result; }
        close_socket(connected);
    }
#ifdef _WIN32
    WSACleanup();
#endif
    return NULL;
}

XLogSocket *xlog_socket_connect(const char *host, uint16_t port, int timeout_ms,
                               const XLogSocketCancellation *token) {
    if (timeout_ms <= 0 || xlog_socket_cancelled(token)) return NULL;
    XLogSocketAddresses *addresses = xlog_socket_resolve(host, port);
    XLogSocket *result = xlog_socket_connect_addresses(addresses, timeout_ms, token);
    xlog_socket_addresses_destroy(addresses);
    return result;
}

int xlog_socket_read(XLogSocket *socket, void *bytes, int capacity, int timeout_ms) {
    if (!socket || !bytes || capacity <= 0 || timeout_ms < 0) return -1;
    int ready = wait_socket(socket->fd, 0, timeout_ms);
    if (ready <= 0) return ready == 0 ? -2 : -1;
    int count = (int)recv(socket->fd, bytes, capacity, 0);
    return count < 0 ? (socket_pending(socket_errno()) ? -2 : -1) : count;
}

int xlog_socket_write(XLogSocket *socket, const void *bytes, int count) {
    if (!socket || !bytes || count <= 0) return -1;
#ifdef MSG_NOSIGNAL
    int flags = MSG_NOSIGNAL;
#else
    int flags = 0;
#endif
    int sent = (int)send(socket->fd, bytes, count, flags);
    return sent < 0 ? (socket_pending(socket_errno()) ? -2 : -1) : sent == 0 ? -1 : sent;
}

#ifdef _WIN32
XLogSocket *xlog_spot_udp_listen(uint16_t port) {
    return port < 1024 ? NULL : xlog_digital_udp_listen("127.0.0.1", port);
}

XLogSocket *xlog_tcp_listen_loopback(uint16_t port, uint16_t *bound_port) {
    WSADATA runtime;
    if (!bound_port || WSAStartup(MAKEWORD(2, 2), &runtime)) return NULL;
    SOCKET fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(port);
    int size = sizeof(addr);
    BOOL exclusive = TRUE;
    if (fd != INVALID_SOCKET && nonblocking(fd) &&
        !setsockopt(fd, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (const char *)&exclusive, sizeof(exclusive)) &&
        !bind(fd, (struct sockaddr *)&addr, sizeof(addr)) && !listen(fd, SOMAXCONN) &&
        !getsockname(fd, (struct sockaddr *)&addr, &size)) {
        XLogSocket *listener = malloc(sizeof(*listener));
        if (listener) { listener->fd = fd; *bound_port = ntohs(addr.sin_port); return listener; }
    }
    if (fd != INVALID_SOCKET) closesocket(fd);
    WSACleanup();
    return NULL;
}

XLogSocket *xlog_tcp_accept_loopback(XLogSocket *listener) {
    SOCKET fd = accept(listener->fd, NULL, NULL);
    if (fd == INVALID_SOCKET) return NULL;
    WSADATA runtime;
    XLogSocket *client = malloc(sizeof(*client));
    if (client && nonblocking(fd) && !WSAStartup(MAKEWORD(2, 2), &runtime)) {
        client->fd = fd;
        return client;
    }
    free(client);
    closesocket(fd);
    return NULL;
}

XLogSocket *xlog_digital_udp_listen(const char *group, uint16_t port) {
    WSADATA winsock;
    if (!group || !port || WSAStartup(MAKEWORD(2, 2), &winsock) != 0) return NULL;
    struct in_addr ip;
    if (inet_pton(AF_INET, group, &ip) != 1) { WSACleanup(); return NULL; }
    uint32_t host = ntohl(ip.s_addr);
    int multicast = (host >> 28) == 14;
    if (!multicast && host != INADDR_LOOPBACK) { WSACleanup(); return NULL; }
    SOCKET fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    BOOL enabled = TRUE;
    struct sockaddr_in address = {0};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = multicast ? htonl(INADDR_ANY) : ip.s_addr;
    address.sin_port = htons(port);
    struct ip_mreq membership = {0};
    membership.imr_multiaddr = ip;
    if (fd != INVALID_SOCKET && nonblocking(fd) &&
        setsockopt(fd, SOL_SOCKET, multicast ? SO_REUSEADDR : SO_EXCLUSIVEADDRUSE, (const char *)&enabled, sizeof(enabled)) == 0 &&
        bind(fd, (struct sockaddr *)&address, sizeof(address)) == 0 &&
        (!multicast || setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, (const char *)&membership, sizeof(membership)) == 0)) {
        XLogSocket *result = malloc(sizeof(*result));
        if (result) { result->fd = fd; return result; }
    }
    if (fd != INVALID_SOCKET) closesocket(fd);
    WSACleanup();
    return NULL;
}

int xlog_digital_udp_read(XLogSocket *socket, void *bytes, int capacity, uint32_t *sender_ip, uint16_t *sender_port) {
    int ready = wait_socket(socket->fd, 0, 50);
    if (ready <= 0) return ready == 0 ? -2 : -1;
    struct sockaddr_in source = {0};
    int len = sizeof(source);
    int count = recvfrom(socket->fd, bytes, capacity, 0, (struct sockaddr *)&source, &len);
    if (count >= 0) {
        if (sender_ip) *sender_ip = ntohl(source.sin_addr.s_addr);
        if (sender_port) *sender_port = ntohs(source.sin_port);
        return count == 0 ? -3 : count;
    }
    int error = WSAGetLastError();
    if (error == WSAEMSGSIZE) return -3;
    return socket_pending(error) ? -2 : -1;
}

int xlog_spot_udp_read(XLogSocket *socket, void *bytes, int capacity) {
    return xlog_digital_udp_read(socket, bytes, capacity, NULL, NULL);
}
#endif

void xlog_socket_close(XLogSocket *socket) {
    if (!socket) return;
    close_socket(socket->fd);
    free(socket);
#ifdef _WIN32
    WSACleanup();
#endif
}
