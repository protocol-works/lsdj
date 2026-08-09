# Model licensing release checklist

Run this checklist for every public release and whenever a model, runtime,
download source, license, terms page, product distribution model, or commercial
status changes. This is a release-control checklist, not legal advice.

## Automated inventory gate

- [ ] `python3 compliance/validate_inventory.py` passes.
- [ ] `python3 -m unittest compliance.test_inventory` passes.
- [ ] `audit_base_revision` is updated to the release candidate's full commit.
- [ ] Each application runtime/package pin matches its manifest entry and lockfile.
- [ ] Every model/processor/LoRA download supplies the exact manifest revision;
      no `main`, `master`, `HEAD`, latest tag, or omitted revision remains.
- [ ] Download verification rejects a source/revision mismatch and partial files.
- [ ] No installer contains a manifest weight/adapter whose
      `redistribution_confirmed` value is false.

## Project-owner record

- [ ] Owners selected and committed the LSDJ code license and notice.
- [ ] Owners confirmed the current open-source/non-commercial project-use path
      for Google MRT2, the PyTorch conversion, Stable Audio 3, and T5Gemma.
- [ ] Owners resolved the Apache model-card vs underlying CC-BY-4.0 treatment for
      both re-keyed PyTorch MRT2 snapshots.
- [ ] Owners confirmed the Stable Audio Community and Gemma derivative path for
      the exact optimized snapshot.
- [ ] Owners either identified specific permission for each official/downloadable
      LoRA or removed it from the official catalog. No sensitive account,
      contract, credential, or revenue information was put in the public record.
- [ ] Any change in product distribution or commercial status triggered a fresh
      owner review before publication.

## Application and package notices

- [ ] About/Licenses lists each manifest asset name, exact revision, upstream
      link, code license, weight/model terms, attribution, and owner-review state.
- [ ] The packaged notices include applicable Apache/MIT notices, Google MRT2
      CC-BY attribution, Stability Community attribution and “Powered by
      Stability AI” display, and the Gemma notice/terms link.
- [ ] LSDJ's code license is visually separate from third-party model terms and
      explicitly says it does not relicense model weights or LoRAs.
- [ ] Release notes link this notice document and state whether models are
      downloaded rather than included.
- [ ] Platform packages (macOS, Linux, Windows) contain the same notice version.

## Download acknowledgement and access

- [ ] First download is blocked until the current asset/license revision is
      acknowledged wherever the manifest says `terms_acceptance_required: true`.
- [ ] A changed asset revision or notice/terms version requires a fresh
      acknowledgement; unrelated telemetry or marketing consent is separate.
- [ ] Gated-download cancellation, offline behavior, rejection, revoked access,
      and expired/invalid credentials produce actionable errors without starting
      a partial install.
- [ ] Tokens are redacted from command lines, logs, events, diagnostics, crash
      reports, and UI; persistent tokens use the OS credential store and are
      never written to plaintext application data.
- [ ] Anonymous optimized artifacts still show the underlying Stability/Gemma
      terms; anonymous access is not treated as permission to relicense.

## LoRA provenance

- [ ] `bundled_lora_ids`, `official_lora_ids`, and documented references match
      the actual application catalog and documentation exactly.
- [ ] Each LSDJ-provided LoRA download displays author, source, exact revision,
      adapter license, compatible base, and base-model terms before download.
- [ ] No LoRA is mirrored or bundled without permission for that exact artifact.
- [ ] User imports show the responsibility/provenance notice and do not claim
      that LSDJ reviewed the user's rights or private file contents.
- [ ] The Motif Maqam adapter remains a user-directed reference unless its
      unresolved license/permission gate is closed.

## Evidence captured for the release

- [ ] Record release tag/commit, inventory version, reviewer, review date, and
      the exact artifact revisions actually downloaded in the private release
      record.
- [ ] Archive generated package file lists proving restricted weights are absent.
- [ ] Record passing acknowledgement-versioning, gated-download blocking,
      offline/error, and credential-redaction tests from the follow-up UI/download
      implementation.
- [ ] If any item above is not complete, block Linux and Windows publication and
      link the unresolved owner decision without exposing sensitive information.
