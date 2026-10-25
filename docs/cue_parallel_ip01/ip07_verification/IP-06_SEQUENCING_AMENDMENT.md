# IP-06 sequencing amendment — 2026-10-25

**This amendment takes precedence only on dependency/order and the identity-PCM ReplayGain observer choice.** Keep every IP-06 correctness, publication, RG-mode, configuration-hash, adverse-case and whole-run performance requirement intact.

## Current execution order

```text
IP-05 — host-accepted, merged; source SHA af66b5c2a40904d4db6fec3fd4185914b4aadd32c74cf6fd6ff15568bf2cbd20
   |
IP-07 — concurrent *source-range* ReplayGain for the admitted identity-PCM S3 band
   |   (implement, prove native numerical/tag equivalence, host-accept)
IP-06 — requalify the now-complete default-RG pilot and only then decide on promotion
```

IP-06's original text and the R2 `provisional/PX-C_stateful_and_fallback.md` assumed a **per-worker decoded-stream tap** promoted only upon IP-06's deferred verdict. The accepted IP-05 host's S1/pilot attribution and the supplied source-range measurements now provide that trigger **without another IP-06 run**. IP-07 therefore replaces that planned *identity-band* observer implementation, **not** PX-C's possible role after future PX-A/PX-B transformed-lossless expansion.

## Rules for the eventual IP-06 session

1. Take the **exact host-accepted IP-07 cumulative source archive** as input (not the unchanged IP-05 snapshot in this planning delivery). Record its actual external SHA-256 and hardware/tool binary versions. If IP-07 is unqualified, remain default OFF; do not claim the precondition was satisfied.
2. Verify S3 with `RG Both` really applies the native concurrent source-range scan, not a post-output scan. Also record the S1 source-pass path and fallback-to-post-output cases. Run matched `RG Off` and `RG Both` trials separately with route-excluded, RG-*included* configuration hashes; a shared hash is required **within a route pair**, not across different RG settings.
3. Preserve the IP-06 >=5 paired primary-cell runs, representative music, exact decoded PCM, ReplayGain tags/album values, logs, staging/visible publication, failures, cancellation, peak resource budgets and 1.25x whole-run promotion threshold. Record post-output fallbacks distinctly. Do not use the 1.088 s standalone source-meter result or the hypothetical ~7 s prediction as completed-implementation timing.
4. Keep the existing deferral/rejection safety options. If IP-07 is correct but loses total wall time under default RG, it is a valid performance outcome: S3 remains OFF by default, and no stream-tap redesign is automatically authorized. The optional observer optimization failing at host does not authorize silent fallback after an encoder failure.
5. Resolve historical IP-06 evaluator defect H-01 (cross-mode common-configuration digest) within IP-06, not in IP-07, unless a correctness bug forces a separately justified correction.

**No production source was changed by issuing this amendment.** The accepted S1 path, opt-in S3 pilot and existing post-output ReplayGain path remain the rollback baseline.
