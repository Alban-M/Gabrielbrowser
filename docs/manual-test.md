# Manual test pass

Every step below was run against the release binary and the output is what it
actually printed. Takes about ten minutes.

This is the **maintainer's** pass — "does this build work" — and it is a
different thing from [docs/preview-1.md](preview-1.md), which is about watching
somebody who has never seen Gabriel. Do not use this script on a preview user:
it tells them the answers.

Needs `curl` and `python3`. No network: the origin is local, so this works on a
plane or an air-gapped machine. One optional step reaches example.com.

## Setup

```sh
mkdir -p /tmp/gabriel-manual && cd /tmp/gabriel-manual
export GABRIEL_VAULT_PASSPHRASE=manual-test   # avoids keychain prompts
```

A tiny origin that requires a session cookie — `/login` issues one, everything
else demands it:

```python
# origin.py
import http.server, json, socketserver

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path.startswith("/login"):
            body = json.dumps({"ok": True}).encode()
            self.send_response(200)
            self.send_header("Set-Cookie", "session_id=abc123; Path=/")
        else:
            cookie = self.headers.get("Cookie", "")
            body = json.dumps({"path": self.path, "authed": "session_id" in cookie}).encode()
            self.send_response(200 if "session_id" in cookie else 401)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True     # threading matters: see the gotcha below
    daemon_threads = True

Server(("127.0.0.1", 8899), Handler).serve_forever()
```

```sh
python3 origin.py &
curl -s http://127.0.0.1:8899/api/me      # {"path": "/api/me", "authed": false}
```

> **Gotcha that cost ten minutes.** A single-threaded `TCPServer` deadlocks
> here. The proxy holds an HTTP/1.1 keep-alive connection open, and a
> single-threaded server cannot then accept anything else, so the replay times
> out and looks like a Gabriel bug. Use `ThreadingTCPServer`.

## 1. A collection

```sh
gabriel init
gabriel ls
```

**Pass:** `created ./gabriel`, and `ls` lists `example`.

## 2. The capture proxy

```sh
gabriel capture start --port 8888 &
```

**Pass:** prints the listen address, the session name, and the path to a CA it
generated. Check the CA is not world-readable:

```sh
ls -l gabriel/.runtime/gabriel-ca.pem     # expect -rw------- (0600)
```

## 3. Record real traffic

```sh
curl -s --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/login -c jar.txt
curl -s --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/api/me -b jar.txt
```

**Pass:** the second returns `"authed": true`.

Optional, proves HTTPS interception (needs network):

```sh
curl -s --proxy http://127.0.0.1:8888 --cacert gabriel/.runtime/gabriel-ca.pem \
  https://example.com -o /dev/null -w "%{http_code}\n"      # 200
```

## 4. Find it

```sh
gabriel capture ls
```

**Pass:** newest first, showing the `/login` and `/api/me` calls with status and
timing.

## 5. Promote — the step the product exists for

```sh
gabriel promote <capture-id> --to users/me
cat gabriel/requests/users/me.toml
```

**Pass, and this is the assertion that matters:**

```sh
grep abc123 gabriel/requests/users/me.toml     # must find NOTHING
```

The file should carry `[auth] type = "session"` instead of a `Cookie` header.
It is safe to commit. Gabriel also notes when it keeps a literal URL because
the environment's `base_url` points elsewhere.

## 6. Replay — still authenticated

```sh
gabriel run users/me
```

**Pass:** `200 OK` and a body containing `"authed": true`. A file with no
credential in it produced an authenticated request.

```sh
gabriel run users/me --quiet | python3 -m json.tool
```

## 7. Diff two responses

```sh
curl -s --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/api/me -b jar.txt
gabriel diff <older-id> <newer-id>
```

**Pass:** `no differences`, having ignored the headers that change every time.
Both ids are required; there is no "diff against the previous one" shorthand.

## 8. Vault, curl, JWT

```sh
gabriel vault set api_token "sk-live-EXAMPLE-1234567890"
gabriel vault ls                 # names only, never values
gabriel curl users/me            # credentials masked unless --show-secrets
gabriel jwt "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMiLCJleHAiOjE3MDAwMDAwMDB9.sig"
```

**Pass:** `vault ls` prints `api_token` and no value. `jwt` decodes the claims,
flags the token as expired, and says the signature is **not** verified.

## 9. HAR round trip

```sh
gabriel har export --out traffic.har
python3 -c "import json;d=json.load(open('traffic.har'));print(len(d['log']['entries']), d['log'].get('comment'))"
gabriel har import traffic.har
```

**Pass:** every capture is exported — no default limit — and `comment` is
`None`, because a complete export has nothing to declare. Then:

```sh
gabriel har export --limit 1 --out partial.har
python3 -c "import json;print(json.load(open('partial.har'))['log']['comment'])"
```

**Pass:** the file itself says `PARTIAL EXPORT: … N were omitted …`. A stderr
warning would not survive the file being attached to a ticket.

Export also warns that the HAR contains credentials verbatim — that is
deliberate and documented; a HAR is a faithful record of traffic.

## 10. Run the collection, report to CI

```sh
gabriel run --all --junit results.xml --html report.html
```

**Pass:** both files are written, `results.xml` has `<testsuite>` elements with
counts, and `report.html` opens in a browser. The starter `example` request
will fail without network — that is a correct failure, and `run --all` should
carry on past it rather than stopping.

## 11. Diagnostics

```sh
gabriel doctor
```

**Pass:** with the proxy still running it should *warn* that port 8888 is in
use and tell you how to pick another. That warning is the check working. Exit
code stays 0 for warnings; 1 only for a real failure.

```sh
gabriel feedback --out bundle
grep -r abc123 bundle/          # must find NOTHING
cat bundle/README.md
```

**Pass:** a directory of text files, the session cookie absent, and a README
listing what was deliberately left out.

## 12. Clean up

```sh
pkill -f origin.py
pkill -f "gabriel capture"
cd / && rm -rf /tmp/gabriel-manual
```

## Not covered here

- **OAuth against a real provider** — see [oauth-interop.md](oauth-interop.md).
  It needs client IDs and a consent screen.
- **WebSocket** (`gabriel ws`) — needs an echo server; covered by the automated
  suite end to end.
- **mTLS** — needs a client certificate; covered by the automated suite.
- **Windows** — none of this has been run there by hand.
