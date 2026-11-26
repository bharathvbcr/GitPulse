# EAS Update Code Signing Certificates (Opus M3)

Per Waylode Mobile Security Gate **M3**, `expo-updates` OTA code signing is **mandatory**.
Malicious or tampered JS bundles MUST NOT be loadable by the app, and security-critical
approval confirmation UI + P-256 hardware signing live exclusively in native/Rust code
(`modules/waylode-crypto/`).

## Generating the Signing Keypair (Offline Only)

Run on an air-gapped or hardware-protected signing workstation — **NEVER** commit or upload
the private key to Git or CI secrets:

```bash
npx expo-updates codesigning:generate \
  --key-output-directory ./private-keys-offline \
  --certificate-output-directory ./certificates \
  --certificate-validity-duration-years 5 \
  --certificate-common-name "Waylode Mobile Release OTA"
```

Only the public X.509 certificate (`./certificates/code-signing-certificate.pem`) is
referenced by `app.config.ts` (`updates.codeSigningCertificate`).
Private keys (`*.key`, `*private*`) are strictly forbidden in the repository and rejected by
`validateMobileAppConfig()`.
