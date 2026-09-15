# Windows Code Signing with SignPath Foundation

This guide explains how to get the Windows build of HuggingCar Fiscal signed, so that
SmartScreen stops warning about an unknown publisher. SignPath Foundation signs open-source
projects for free with an HSM-backed OV certificate issued in the Foundation's name.

macOS notarization and Linux are not covered: `.deb` packages are not signed at this level,
and Apple signing needs a paid Developer ID (see the last section).

## Prerequisites

- **Public repository under an OSI licence** — `HuggingCar/workshop` is public and MIT
  licensed.
- **At least one published release** — the application form asks for a download URL that
  must resolve: <https://github.com/HuggingCar/workshop/releases/latest>.
- **Builds produced only by GitHub Actions on GitHub-hosted runners** — SignPath verifies
  the artifact was uploaded by a workflow run, not by a person. `release.yml` already does
  this.
- **Organization admin on GitHub** — to install the SignPath GitHub App and add secrets.

> **Note:** SignPath prefers projects with visible users. A brand-new repository can be
> declined with "reapply later"; the *Reputation* field carries the application.

## 1. Apply

Fill in <https://signpath.org/apply>. The form is a HubSpot embed; these are its fields:

| Field | Value |
| --- | --- |
| Project Name | `HuggingCar workshop` |
| Repository URL | `https://github.com/HuggingCar/workshop` |
| Homepage URL | `https://github.com/HuggingCar/workshop` (or `https://huggingcar.com` if that page links to the repository — reviewers check the link) |
| Download URL | `https://github.com/HuggingCar/workshop/releases/latest` |
| Privacy Policy URL | empty; the desktop app sends nothing anywhere |
| Wikipedia URL | empty |
| Tagline | `Desktop app and driver for Posnet Temo Online fiscal printers used in Polish car workshops` |
| Description | `HuggingCar Fiscal is a Python/Qt desktop application that prints fiscal receipts and daily, monthly and periodic reports on Posnet Temo Online printers over USB. The repository also contains the pure-Python Posnet protocol driver and a headless agent that prints receipts queued from the HuggingCar workshop management system. MIT licensed. Windows, macOS and Linux builds are produced by GitHub Actions from every tagged release; we are applying to sign the Windows executable.` |
| Reputation | `Published in September 2026 as the open-source part of HuggingCar, a car-workshop management platform (huggingcar.com) used in production by Polish workshops. Maintained by the HuggingCar team, who also run the backend the agent talks to. All commits go through pull requests with required CI; releases are built only by GitHub Actions.` Add real numbers (workshops, users) if available. |
| Maintainer Type | company / organization, not individual |
| First Name, Last Name, Email | the applicant; use an `@huggingcar.com` address, matching the organization domain helps |
| Company Name | `HuggingCar` |
| Discovery source | optional |
| Code of Conduct checkbox | required — read <https://signpath.org/terms> first; key terms: builds only by CI from the public repository, the signed binary must be exactly what the repository produces, certificates may be revoked on violation |
| Data processing checkbox | required |
| Other communications | your choice |

## 2. After Approval

SignPath creates an organization on <https://app.signpath.io> and emails an invitation.
Collect these values from the SignPath web UI:

1. **Organization ID** — Organization → Settings.
2. **Project slug** — create a project for the repository (or use the one SignPath
   pre-created); the slug is in the project URL.
3. **Signing policy slug** — the project has a `release-signing` policy by default. This
   is the one that produces a trusted signature. For Foundation projects the request must
   be approved by a SignPath reviewer the first few times.
4. **Artifact configuration** — describes what is inside the artifact. The Windows build
   is a single `.exe` inside the ZIP that `actions/upload-artifact` produces:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="huggingcar-fiscal-win-x64.exe">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

5. **API token** — create a CI user in the organization with *Submitter* permission on the
   project, generate its API token.
6. **Install the SignPath GitHub App** — <https://github.com/apps/signpath>, grant access
   to `HuggingCar/workshop`. Needed for SignPath to read the workflow run.
7. **Add the Trusted Build System** — in SignPath: Organization → Trusted Build Systems →
   add *GitHub.com* and link it to the project.

## 3. GitHub Secrets and Variables

In the repository: **Settings → Secrets and variables → Actions**.

| Name | Kind | Value |
| --- | --- | --- |
| `SIGNPATH_API_TOKEN` | secret | the API token from step 2.5 |
| `SIGNPATH_ORGANIZATION_ID` | variable | the organization ID |
| `SIGNPATH_PROJECT_SLUG` | variable | the project slug |
| `SIGNPATH_SIGNING_POLICY_SLUG` | variable | `release-signing` |

## 4. Workflow Changes

Add the signing step to the Windows leg of `release.yml`, between `build.sh` and the
final artifact upload. The unsigned executable must first be uploaded as a workflow
artifact — SignPath fetches it from GitHub rather than trusting anything the job sends —
then the signed file replaces it.

```yaml
      - if: runner.os == 'Windows'
        id: unsigned
        uses: actions/upload-artifact@v7
        with:
          name: win-x64-unsigned
          path: packages/desktop/release/huggingcar-fiscal-win-x64.exe
          retention-days: 1

      - if: runner.os == 'Windows'
        uses: signpath/github-action-submit-signing-request@v2
        with:
          api-token: ${{ secrets.SIGNPATH_API_TOKEN }}
          organization-id: ${{ vars.SIGNPATH_ORGANIZATION_ID }}
          project-slug: ${{ vars.SIGNPATH_PROJECT_SLUG }}
          signing-policy-slug: ${{ vars.SIGNPATH_SIGNING_POLICY_SLUG }}
          github-artifact-id: ${{ steps.unsigned.outputs.artifact-id }}
          wait-for-completion: true
          output-artifact-directory: packages/desktop/release
```

The signed `.exe` lands in `packages/desktop/release/` under the same name, so the
existing upload and `publish` steps pick it up unchanged.

Gate the two steps on the secret being present if pull-request builds from forks should
keep working unsigned:

```yaml
        if: runner.os == 'Windows' && secrets.SIGNPATH_API_TOKEN != ''
```

## 5. Verify

After the first signed release:

```powershell
Get-AuthenticodeSignature .\huggingcar-fiscal-win-x64.exe | Format-List
```

`Status` must be `Valid` and `SignerCertificate.Subject` must name *SignPath Foundation*.
SmartScreen reputation for an OV certificate builds up over downloads; the "unknown
publisher" prompt disappears after a while, not immediately.

## Branch Rulesets

SignPath can also enforce *source code and build policies* per signing policy
(`.signpath/policies/<project>/<policy>.yml`, see
<https://docs.signpath.io/trusted-build-systems/github#define-policies-for-source-code-and-builds>).
They are optional and only available on Advanced Code Signing / Code Signing Gateway
plans, so the Foundation subscription does not require them and the current `master`
ruleset (pull request required, squash only, no force push, no deletion, `Static
analysis` and `Test suite` required, organization admins may bypass, no required
approvals) is sufficient to be signed.

If such a policy is ever adopted, its example expects `allow_bypass_actors: false` and
`require_pull_request` with at least one approval; both would need to be tightened in the
repository ruleset first, and `enforced_from: EARLIEST` would only hold from the date of
that change.

## Alternatives

- **Azure Artifact Signing** (formerly Trusted Signing): about 10 USD per month, no
  certificate to buy, official `azure/trusted-signing-action`. Requires an Azure tenant
  and a company with three years of verifiable history. The right choice if the repository
  ever goes private.
- **Commercial OV/EV certificate** (Sectigo, DigiCert, SSL.com): 200–400 USD per year,
  private key on a hardware token or the CA's cloud HSM. EV skips SmartScreen reputation
  building. Only worth it for an unrelated reason.
- **macOS**: Apple Developer Program (99 USD per year) for a *Developer ID Application*
  certificate plus notarization. In `build.sh`: `codesign --options runtime` on the
  `.app`, `xcrun notarytool submit --wait` on the `.dmg`, `xcrun stapler staple`.
  Secrets: base64 `.p12`, its password, Apple ID, app-specific password, Team ID.
