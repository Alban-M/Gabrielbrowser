## Downloads

Each archive is named for its platform — `gabriel-aarch64-apple-darwin.tar.gz`,
`gabriel-x86_64-unknown-linux-gnu.tar.gz`, and so on. The version is this
release, not the filename, so the installer can fetch the newest build without
knowing its number first.

## Verifying this download

Every artifact carries a SHA-256 in `checksums.txt` and a Sigstore signature.
The signature is keyless: the certificate binds the artifact to the workflow
that built it, so there is no signing key for anyone to steal or leak.

```sh
sha256sum -c checksums.txt --ignore-missing

cosign verify-blob \
  --certificate gabriel-<target>.tar.gz.pem \
  --signature   gabriel-<target>.tar.gz.sig \
  --certificate-identity-regexp 'https://github.com/Alban-M/Gabrielbrowser/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  gabriel-<target>.tar.gz
```

The identity names the repository the binaries are built from. It is a private
repository, so the link will 404 for you — that is expected, and it does not
weaken the check: what you are verifying is that this artifact came out of that
workflow and not from somewhere else.

`gabriel-sbom.cdx.json` is a CycloneDX bill of materials listing every
dependency in the shipped binary with its licence and package URL.

## Installing

```sh
curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | sh
```

The installer works out your platform, verifies the SHA-256 before copying
anything, and refuses to install a download it cannot verify. Set
`GABRIEL_INSTALL_DIR` to choose where it lands.

Then:

```sh
gabriel doctor
```

Binaries are **not** code-signed or notarized yet, so macOS will warn about an
unidentified developer. The installer clears the quarantine attribute;
Gatekeeper may still ask once.

## This is a preview

Please report what breaks — particularly anything the installer or `gabriel
doctor` got wrong, since those are the two things that decide whether Gabriel
is usable at all on a machine that is not the one it was built on.
