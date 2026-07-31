# Gabriel — releases

**From real traffic to secure, repeatable API tests — in minutes.**

Gabriel records what your application actually sends, turns any of it into an
editable request with the credentials stripped out, and replays it still
authenticated. The same file then runs in CI.

```text
browse → capture → promote → replay (with the session intact) → diff
```

This repository holds **binaries only** — no source. Releases, checksums,
signatures, and a software bill of materials for each build.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | sh
```

Then:

```sh
gabriel doctor
```

`gabriel doctor` checks the things that actually break a first run and prints
the fix for each one. If Gabriel misbehaves, run it before anything else.

If it does not explain the problem, `gabriel feedback` writes a directory of
diagnostics you can read and then attach to a bug report. Nothing is sent
anywhere — there is no telemetry in Gabriel at all.

The installer works out your platform, verifies the SHA-256 before copying
anything, and refuses to install a download it cannot verify. Override the
defaults with `GABRIEL_INSTALL_DIR` and `GABRIEL_VERSION`.

On Windows, download the `.zip` from the [latest
release](https://github.com/Alban-M/gabriel-releases/releases) and put
`gabriel.exe` on your PATH.

## Verifying a download

Every artifact carries a SHA-256 in `checksums.txt` and a keyless Sigstore
signature. The certificate binds the artifact to the workflow that built it, so
there is no signing key for anyone to steal.

```sh
sha256sum -c checksums.txt --ignore-missing

cosign verify-blob \
  --certificate gabriel-<target>.tar.gz.pem \
  --signature   gabriel-<target>.tar.gz.sig \
  --certificate-identity-regexp 'https://github.com/Alban-M/Gabrielbrowser/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  gabriel-<target>.tar.gz
```

The identity names the repository the binaries are built from. It is public, so
you can read the workflow that produced them. What the check proves is that the
artifact came out of that workflow and nowhere else.

`gabriel-sbom.cdx.json` is a CycloneDX bill of materials listing every
dependency in the shipped binary with its licence and package URL.

## Five minutes

```sh
mkdir demo && cd demo
gabriel init                          # creates ./gabriel/
gabriel capture start                 # a proxy on 127.0.0.1:8888
```

Trust the CA once — `gabriel ca` prints the exact command for your platform —
then point a browser or `curl` at the proxy and use the site you are debugging.
Back in the first terminal:

```sh
gabriel capture ls                    # newest first
gabriel promote <capture-id> --to users/me
gabriel run users/me                  # still authenticated
```

The promoted file has **no credentials in it**: the `Cookie` header becomes a
reference to the session the browser established, and a captured bearer token
moves into an encrypted vault. It is safe to commit, and the replay still
works.

## This is a preview

Binaries are not yet code-signed or notarized, so macOS will warn about an
unidentified developer once. The file format may still change.

Known gaps: OAuth PKCE is verified against a local identity provider but not
yet against Google, GitHub or Auth0; WebSocket frames and streamed bodies pass
through the capture proxy unrecorded; JWT signatures are decoded and checked for
expiry but not cryptographically verified; there is no desktop UI.

Please report what breaks — especially anything the installer or `gabriel
doctor` gets wrong, since those decide whether Gabriel works at all on a machine
that is not the one it was built on.

## Licence

Apache-2.0.
