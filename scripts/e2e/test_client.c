/* LocalProxy e2e 测试客户端。
 *
 * 一个标准 BSD socket HTTP 客户端（getaddrinfo + connect + read/write），
 * 无 hardened runtime、无 SIP 限制，可被 shim 注入。
 *
 * 用法：
 *   test_client [host] [port]        HTTP 探测
 *   test_client --udp <host> <port>  UDP 探测（断言 sendto 结果）
 *
 * 输出：stderr 打印解析与连接过程；stdout 打印 HTTP 响应。
 */

#include <arpa/inet.h>
#include <fcntl.h>
#include <poll.h>
#include <errno.h>
#include <netdb.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/// UDP 探测：sendto 到目标，打印 errno 与结果。
static int udp_probe(const char *host, const char *port) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_DGRAM;

    struct addrinfo *res = NULL;
    int gai = getaddrinfo(host, port, &hints, &res);
    if (gai != 0 || res == NULL) {
        fprintf(stderr, "getaddrinfo failed: %s\n", gai_strerror(gai));
        return 1;
    }

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) {
        perror("socket");
        freeaddrinfo(res);
        return 1;
    }

    const char *payload = "LP-UDP-PROBE";
    ssize_t n = sendto(fd, payload, strlen(payload), 0, res->ai_addr, res->ai_addrlen);
    if (n < 0) {
        printf("UDP_SENDTO_FAILED errno=%d (%s)\n", errno, strerror(errno));
        fprintf(stderr, "udp sendto failed errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        freeaddrinfo(res);
        return 2; // 被阻断（预期：公网目标 block）
    }
    printf("UDP_SENDTO_OK bytes=%zd\n", n);
    fprintf(stderr, "udp sendto ok bytes=%zd\n", n);
    close(fd);
    freeaddrinfo(res);
    return 0;
}

/// 保留已连的 UDP socket，等待测试脚本切换策略后再尝试发送。
static int udp_live_probe(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        return 1;
    }
    struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(3478)};
    inet_pton(AF_INET, "203.0.113.36", &addr.sin_addr);
    if (connect(fd, (const struct sockaddr *)&addr, sizeof(addr)) != 0) {
        perror("UDP connect");
        close(fd);
        return 1;
    }
    puts("UDP_CONNECTED");
    fflush(stdout);
    if (getchar() == EOF) {
        close(fd);
        return 1;
    }
    // 若阻断回归，内核仍会拒绝无效缓冲区，测试不会发出公网报文。
    struct iovec iov = {.iov_base = (void *)(uintptr_t)1, .iov_len = 1};
    struct msghdr message = {.msg_iov = &iov, .msg_iovlen = 1};
    ssize_t sent = sendmsg(fd, &message, 0);
    int error = errno;
    close(fd);
    if (sent == -1 && error == ENETUNREACH) {
        return 0;
    }
    fprintf(stderr, "已连接的 UDP socket 未按新策略阻断: sendmsg=%zd errno=%d\n", sent, error);
    return 1;
}

/// DNS 探测：gethostbyname 打印 h_name 与首个地址（验证 FakeIP 覆盖旁路 API）。
static int dns_probe(const char *host) {
    struct hostent *he = gethostbyname(host);
    if (he == NULL) {
        printf("GETHOSTBYNAME_NULL\n");
        return 1;
    }
    char ipstr[64] = {0};
    if (he->h_addr_list != NULL && he->h_addr_list[0] != NULL) {
        inet_ntop(he->h_addrtype, he->h_addr_list[0], ipstr, sizeof(ipstr));
    }
    printf("GETHOSTBYNAME name=%s addr=%s\n", he->h_name ? he->h_name : "(null)", ipstr);
    return 0;
}


/// 非阻塞 HTTP 探测：O_NONBLOCK + connect(EINPROGRESS) + poll 可写 + SO_ERROR。
/// 验证 shim 对非阻塞 connect 的适配（Electron/Chromium 默认模式）。
static int nonblock_probe(const char *host, const char *port) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    struct addrinfo *res = NULL;
    int gai = getaddrinfo(host, port, &hints, &res);
    if (gai != 0 || res == NULL) {
        fprintf(stderr, "getaddrinfo failed: %s\n", gai_strerror(gai));
        return 1;
    }

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) {
        perror("socket");
        freeaddrinfo(res);
        return 1;
    }

    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);

    int rc = connect(fd, res->ai_addr, res->ai_addrlen);
    if (rc != 0 && errno != EINPROGRESS) {
        printf("NONBLOCK_CONNECT_FAILED errno=%d (%s)\n", errno, strerror(errno));
        perror("connect");
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    fprintf(stderr, "nonblock connect rc=%d errno=%d (%s)\n", rc, errno, strerror(errno));

    struct pollfd pfd = {.fd = fd, .events = POLLOUT};
    int pr = poll(&pfd, 1, 10000);
    if (pr <= 0) {
        printf("NONBLOCK_POLL_TIMEOUT\n");
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    int so_error = 0;
    socklen_t slen = sizeof(so_error);
    getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &slen);
    if (so_error != 0) {
        printf("NONBLOCK_SO_ERROR errno=%d (%s)\n", so_error, strerror(so_error));
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    // 恢复阻塞后发送请求
    fcntl(fd, F_SETFL, flags);

    char request[512];
    snprintf(request, sizeof(request),
             "GET /e2e-test HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", host);
    if (write(fd, request, strlen(request)) < 0) {
        perror("write");
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n > 0) {
        buf[n] = '\0';
        fputs(buf, stdout);
    }
    close(fd);
    freeaddrinfo(res);
    return 0;
}


/// UDP 代理探测：sendto + recvfrom roundtrip（S2 全代理验证）。
static int udp_proxy_probe(const char *host, const char *port) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_DGRAM;

    struct addrinfo *res = NULL;
    int gai = getaddrinfo(host, port, &hints, &res);
    if (gai != 0 || res == NULL) {
        fprintf(stderr, "getaddrinfo failed: %s\n", gai_strerror(gai));
        return 1;
    }

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) {
        perror("socket");
        freeaddrinfo(res);
        return 1;
    }

    const char *payload = "LP-UDP-PROXY-PROBE";
    ssize_t n = sendto(fd, payload, strlen(payload), 0, res->ai_addr, res->ai_addrlen);
    if (n < 0) {
        printf("UDP_PROXY_SEND_FAILED errno=%d (%s)\n", errno, strerror(errno));
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    printf("UDP_PROXY_SEND_OK bytes=%zd\n", n);

    // 收响应（roundtrip）
    struct sockaddr_storage from;
    socklen_t fromlen = sizeof(from);
    char buf[4096];
    ssize_t r = recvfrom(fd, buf, sizeof(buf) - 1, 0, (struct sockaddr *)&from, &fromlen);
    if (r > 0) {
        buf[r] = '\0';
        printf("UDP_PROXY_RECV data=%s\n", buf);
    } else {
        printf("UDP_PROXY_RECV_FAILED errno=%d (%s)\n", errno, strerror(errno));
    }
    close(fd);
    freeaddrinfo(res);
    return 0;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "--udp-live") == 0) {
        return udp_live_probe();
    }
    if (argc >= 2 && strcmp(argv[1], "--udp") == 0) {
        const char *host = argc > 2 ? argv[2] : "example.com";
        const char *port = argc > 3 ? argv[3] : "3478";
        return udp_probe(host, port);
    }
    if (argc >= 2 && strcmp(argv[1], "--udp-proxy") == 0) {
        const char *host = argc > 2 ? argv[2] : "example.com";
        const char *port = argc > 3 ? argv[3] : "3478";
        return udp_proxy_probe(host, port);
    }
    if (argc >= 2 && strcmp(argv[1], "--nonblock") == 0) {
        const char *host = argc > 2 ? argv[2] : "example.com";
        const char *port = argc > 3 ? argv[3] : "80";
        return nonblock_probe(host, port);
    }
    if (argc >= 2 && strcmp(argv[1], "--dns") == 0) {
        const char *host = argc > 2 ? argv[2] : "example.com";
        return dns_probe(host);
    }

    const char *host = argc > 1 ? argv[1] : "example.com";
    const char *port = argc > 2 ? argv[2] : "80";

    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    struct addrinfo *res = NULL;
    int gai = getaddrinfo(host, port, &hints, &res);
    if (gai != 0 || res == NULL) {
        fprintf(stderr, "getaddrinfo failed: %s\n", gai_strerror(gai));
        return 1;
    }

    char ipstr[64] = {0};
    getnameinfo(res->ai_addr, res->ai_addrlen, ipstr, sizeof(ipstr), NULL, 0,
                NI_NUMERICHOST);
    fprintf(stderr, "resolved %s -> %s\n", host, ipstr);

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) {
        perror("socket");
        freeaddrinfo(res);
        return 1;
    }
    if (connect(fd, res->ai_addr, res->ai_addrlen) != 0) {
        perror("connect");
        close(fd);
        freeaddrinfo(res);
        return 1;
    }
    fprintf(stderr, "connected fd=%d\n", fd);

    char request[512];
    snprintf(request, sizeof(request),
             "GET /e2e-test HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",
             host);
    if (write(fd, request, strlen(request)) < 0) {
        perror("write");
        close(fd);
        freeaddrinfo(res);
        return 1;
    }

    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n > 0) {
        buf[n] = '\0';
        fputs(buf, stdout);
    }
    close(fd);
    freeaddrinfo(res);
    return 0;
}
