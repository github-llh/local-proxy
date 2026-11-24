/* LocalProxy e2e 测试客户端。
 *
 * 一个标准 BSD socket HTTP 客户端（getaddrinfo + connect + read/write），
 * 无 hardened runtime、无 SIP 限制，可被 shim 注入。
 *
 * 用法：test_client [host] [port]
 * 输出：stderr 打印解析与连接过程；stdout 打印 HTTP 响应。
 */

#include <netdb.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(int argc, char **argv) {
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
