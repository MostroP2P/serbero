<!--
Cooperative self-resolution invitation templates (Feature 005).

This file is parsed as TOML by `crate::prompts::self_resolution_parser`.
HTML-style comments (like this block) are stripped before parsing.

POLICY BOUNDARIES (enforced by the keyword-audit unit test in
`tests/phase3_self_resolution_template_audit.rs`):

  - The `template` and `human_assistance_optin` strings MUST NOT name
    or imply ANY fund-moving action: release, settle, cancel,
    disburse, transfer, refund, pay, send fiat, send sats, etc.
    The Phase 3 authority boundary documented in
    `prompts/phase3-system.md` continues to apply; this file's
    contents are an extension of that boundary into a templated,
    LLM-static surface.

  - Translations MUST preserve the neutral, non-directive tone of the
    English original. Confirming a factual claim from either party
    (e.g. "we acknowledge you received the fiat") is forbidden — the
    Phase 3 system prompt's neutrality rule applies here too.

  - To add a new language: append a new `[xx]` section AND extend the
    banned-substring matrix in
    `tests/phase3_self_resolution_template_audit.rs`. Both changes
    must land in the same PR so a reviewer can confirm the audit
    covers the new translation.

The `fallback_language` is the language used when the classifier
could not determine a party's language confidently (`buyer_language`
/ `seller_language` are `null` on the structured response) OR when
the detected code has no matching `[xx]` section in this file.
-->

fallback_language = "en"

[en]
template = "Thanks for the update — it sounds like the two of you may be close to coordinating the next step between yourselves. I'll keep monitoring this conversation in case anything changes."
human_assistance_optin = "If you'd prefer human assistance instead, just let me know in this chat and I'll route you to the assigned solver."

[es]
template = "Gracias por la actualización: parece que ustedes dos podrían estar cerca de coordinar el siguiente paso entre sí. Sigo atento a esta conversación por si algo cambia."
human_assistance_optin = "Si prefieres asistencia humana, dímelo en este chat y te conecto con la persona asignada al caso."

[pt]
template = "Obrigado pela atualização — parece que vocês dois podem estar perto de coordenar o próximo passo entre si. Continuo acompanhando esta conversa caso algo mude."
human_assistance_optin = "Se preferir assistência humana, me avise neste chat e eu encaminho você para a pessoa designada."
