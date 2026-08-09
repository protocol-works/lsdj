# Model and runtime asset inventory

`model-assets.json` is the machine-readable source of truth for the model and
model-runtime compliance work in issue #108. It records what upstream projects
say at exact revisions, how LSDJ obtains each asset, and which decisions still
require project-owner review. It does not approve a use or provide legal advice.

Run the dependency-free validator and its mutation tests with:

```sh
python3 compliance/validate_inventory.py
python3 -m unittest compliance.test_inventory
```

The validator fails for missing required fields, branch-like or short revisions,
revision URLs that do not contain the pinned hash, unsafe installer settings,
unresolved dependency/catalog IDs, and mutable runtime download behavior that is
not kept as a release gate. An upstream source revision may be recorded as
`unresolved_upstream` only with a null value, a canonical evidence URL, and an
explicit release gate; this avoids inventing precision the reviewed evidence does
not support.

When a runtime or model revision changes, update this manifest, the human notice
document, and the release checklist in the same pull request. Evidence URLs for
versioned source/model artifacts should point at the exact commit or snapshot;
policy URLs may remain canonical because upstream policies are living documents.
