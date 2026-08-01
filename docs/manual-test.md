# Manual test pass

**A local maintainer smoke pass**, not a release verification. It builds from
the working tree and exercises it. That is a different claim from "the published
artifact works" — which is what the release workflow's smoke job asserts, by
downloading the artifact, verifying its checksum and installing it before
anything is signed.

If you want to test the *release*, install the downloaded archive and run these
steps against that binary instead. Do not build locally and call the result a
release check.

Every step below was run against the binary built from this tree, and the output
is what it actually printed. Takes about ten minutes.

This is the **maintainer's** pass — "does this build work" — and it is a
different thing from [docs/preview-1.md](preview-1.md), which is about watching
somebody who has never seen Gabriel. Do not use this script on a preview user:
it tells them the answers.

Needs `curl` and `python3`. No network: the origin is local, so this works on a
plane or an air-gapped machine. One optional step reaches example.com.

## Setup

**Put the binary on your PATH first.** `cargo build` leaves it in `target/`,
where the shell will not find it — the first thing this script did on a clean
machine was print `zsh: command not found: gabriel` five times.

```sh
cargo build --release
mkdir -p ~/.local/bin
install -m 755 target/release/gabriel ~/.local/bin/gabriel
gabriel --version                     # expect: gabriel 0.1.0-preview.1
```

If `~/.local/bin` is not on your PATH, either add it or use
`export PATH="$PWD/target/release:$PATH"` for this session.

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
ORIGIN_PID=$!
curl -s http://127.0.0.1:8899/api/me      # {"path": "/api/me", "authed": false}
```

Keep the pid. `pkill -f origin.py` at the end would also kill an unrelated
`origin.py` somebody else is running, and `pkill -f "gabriel capture"` would kill
a capture session that has nothing to do with this test.

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
PROXY_PID=$!
sleep 2
lsof -nP -iTCP:8888 -sTCP:LISTEN >/dev/null || { echo "the proxy did not start"; exit 1; }
```

**Check that it bound before going on.** If the port is already held — by a
capture session somebody forgot to stop — Gabriel prints
`could not listen on 127.0.0.1:8888: Address already in use` and exits, but a
backgrounded process with its output redirected takes that message with it.
Every step after this one would then pass while testing nothing: curl would
reach the *other* proxy, and the captures would land in somebody else's
collection. Writing this document, that is exactly what happened.

**Pass:** prints the listen address, the session name, and the path to a CA it
generated. Check the permissions:

```sh
ls -l gabriel/.runtime/gabriel-ca.key     # expect -rw-------  (0600)
ls -l gabriel/.runtime/gabriel-ca.pem     # expect -rw-r--r--  (0644)
```

Two files, two correct answers. The **key** must be private — that is the one
whose leak lets somebody forge certificates for any site. The **certificate**
is public by design and is *supposed* to be readable, since curl and browsers
have to read it to trust it. An earlier version of this document checked the
`.pem` for `0600` and reported a failure against correct behaviour.

## 3. Record real traffic

```sh
curl -s --noproxy '' --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/login -c jar.txt
curl -s --noproxy '' --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/api/me -b jar.txt
```

**Pass:** the second returns `"authed": true`.

`--noproxy ''` is load-bearing. Many machines set
`NO_PROXY=localhost,127.0.0.1`, which makes curl ignore `--proxy` for exactly
these addresses — the requests would succeed, nothing would be captured, and the
test would report a pass for a proxy it never went through. A test that can
silently lie is worse than no test.

Optional, proves HTTPS interception (needs network):

```sh
curl -s --noproxy '' --proxy http://127.0.0.1:8888 --cacert gabriel/.runtime/gabriel-ca.pem \
  https://example.com -o /dev/null -w "%{http_code}\n"      # 200
```

## 4. Find it

```sh
gabriel capture ls
```

**Pass:** newest first, showing the `/login` and `/api/me` calls with status and
timing.

## 5. Promote — the step the product exists for

Take the id from step 4. Substituted here rather than written as
`<capture-id>`, because a placeholder pasted verbatim becomes
`zsh: no such file or directory: capture-id`:

```sh
CAPTURE=$(gabriel capture ls | grep '/api/me' | head -1 | awk '{print $1}')
gabriel promote "$CAPTURE" --to users/me
cat gabriel/requests/users/me.toml
```

**Pass, and this is the assertion that matters:**

```sh
test -f gabriel/requests/users/me.toml && \
  ! grep -q abc123 gabriel/requests/users/me.toml && \
  echo "PASS — the file exists and has no credential in it"
```

Written as one assertion rather than `grep abc123 …` on its own, because a
missing file makes a bare grep print a warning and produce no matches — which
reads exactly like success to somebody scanning for "no output". *Absent* and
*clean* are different results and the check has to tell them apart. This is the
central claim of the product; it should not be the loosest test in the
document.

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

## 7. Replaying something that changes the world

The step this document did not have until the safety model existed — and the one
most likely to look like a bug if you have not seen it.

```sh
curl -s --noproxy '' --proxy http://127.0.0.1:8888 -X POST \
  http://127.0.0.1:8899/orders -b jar.txt
sleep 1
POSTED=$(gabriel capture ls | grep orders | head -1 | awk '{print $1}')
gabriel promote "$POSTED" --to orders/place
gabriel run orders/place < /dev/null
```

**Pass — and the pass is a refusal:**

```
careful: `orders/place` is POST unsafe — performs the action again — a second one
error: refusing to run `orders/place` without a terminal to ask at.
```

Nothing was ordered. Redirecting from `/dev/null` is what a pipeline or a CI job
looks like; Gabriel will not guess on your behalf where nobody can answer.

```sh
gabriel run orders/place --dry-run    # resolves everything, sends nothing
gabriel run orders/place --yes        # actually places the order
gabriel run --all < /dev/null         # runs the reads, skips the rest, says how many
```

**Pass:** the dry run prints the real payload and the order count does not move;
`--yes` returns `201` and it does; `run --all` finishes with
`note: 1 of N request(s) were not run — you declined to repeat them`.

At a terminal it prompts instead. `PUT` and `DELETE` are announced too — the
first replay of a `DELETE` still deletes, so idempotent is not treated as safe.

## 8. Credentials are masked where you would paste them

```sh
gabriel curl users/me | grep -i cookie
gabriel run orders/place --dry-run | grep -i cookie
```

**Pass:** both print `••••redacted••••`, never `session_id=abc123`.

This is worth its own step because it failed once. The redactor masks values it
was *told* are secrets — everything resolved from the vault — and a session
cookie never goes through the vault, so it was printed in full by two surfaces
whose whole purpose is being pasted somewhere. Credential-carrying headers are
masked by name now.

```sh
gabriel curl users/me --show-secrets | grep -i cookie
```

**Pass:** the real value, because that is what was asked for.

## 9. Diff two responses

```sh
curl -s --noproxy '' --proxy http://127.0.0.1:8888 http://127.0.0.1:8899/api/me -b jar.txt
NEWER=$(gabriel capture ls | grep '/api/me' | sed -n '1p' | awk '{print $1}')
OLDER=$(gabriel capture ls | grep '/api/me' | sed -n '2p' | awk '{print $1}')
gabriel diff "$OLDER" "$NEWER"
```

**Pass:** `no differences`, having ignored the headers that change every time.
Both ids are required; there is no "diff against the previous one" shorthand.

## 10. Vault, curl, JWT

```sh
gabriel vault set api_token "sk-live-EXAMPLE-1234567890"
gabriel vault ls                 # names only, never values
gabriel curl users/me            # credentials masked unless --show-secrets
gabriel jwt "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMiLCJleHAiOjE3MDAwMDAwMDB9.sig"  # scan-secrets:allow — a fabricated token, signature literally "sig"
```

**Pass:** `vault ls` prints `api_token` and no value. `jwt` decodes the claims,
flags the token as expired, and says the signature is **not** verified.

## 11. HAR round trip

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

## 12. Run the collection, report to CI

```sh
gabriel run --all --junit results.xml --html report.html
```

**Pass:** both files are written, `results.xml` has `<testsuite>` elements with
counts, and `report.html` opens in a browser. The starter `example` request
will fail without network — that is a correct failure, and `run --all` should
carry on past it rather than stopping.

## 13. Diagnostics

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

## 14. Clean up

```sh
kill "$PROXY_PID" "$ORIGIN_PID"
cd / && rm -rf /tmp/gabriel-manual
```

## Not covered here

- **OAuth against a real provider** — see [oauth-interop.md](oauth-interop.md).
  It needs client IDs and a consent screen.
- **WebSocket** (`gabriel ws`) — needs an echo server; covered by the automated
  suite end to end.
- **mTLS** — needs a client certificate; covered by the automated suite.
- **Windows** — none of this has been run there by hand.
