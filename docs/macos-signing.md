# macOS opening and signing

The current `.app` packages are ad-hoc signed. This records a valid code
signature, but it does not establish a trusted Developer ID and does not
notarize the app. Gatekeeper may still block a downloaded copy.

If you trust the downloaded release, try **Control-click → Open** on the app.
If macOS still reports that it is damaged, remove the quarantine attribute from
that exact app bundle and open it again:

```sh
xattr -dr com.apple.quarantine "/Applications/SQL Manager.app"
open "/Applications/SQL Manager.app"
```

## Developer ID signing and notarization

To remove the first-launch Gatekeeper warning, the release needs an Apple
Developer ID Application certificate and notarization. Do not commit the
certificate, private key, or passwords.

1. Export the Developer ID Application certificate and private key from
   Keychain Access as a `.p12` file.
2. Add these repository Actions secrets:
   - `APPLE_DEVELOPER_ID_P12_BASE64`
   - `APPLE_DEVELOPER_ID_P12_PASSWORD`
   - `APPLE_TEAM_ID`
   - `APPLE_ID`
   - `APPLE_APP_SPECIFIC_PASSWORD`
3. In the macOS packaging job, decode the `.p12` into a temporary file, set
   cargo-bundle's `apple_signing_p12`, `apple_signing_password_env`, and
   `apple_signing_hardened_runtime` metadata, and use the signing identity from
   the certificate instead of ad-hoc signing.
4. After building the DMG, submit both the app and DMG with `xcrun notarytool`
   using the Apple ID/team credentials, wait for an accepted result, then run
   `xcrun stapler staple` on both artifacts before upload.
5. Verify the final app with `codesign --verify --deep --strict` and
   `spctl --assess --type execute --verbose` on a Mac runner.

An App Store Connect API key can replace the Apple ID/app-specific-password
pair; store its key ID, issuer ID, and `.p8` contents as protected Actions
secrets. Keep the current ad-hoc workflow until those signing secrets are
configured and the signed/notarized pipeline passes on macOS.
