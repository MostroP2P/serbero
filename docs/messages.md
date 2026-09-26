# Messages

Every word Serbero sends is defined here. Party-facing messages are templates
chosen by the decision table ([judgments.md §4](judgments.md#4-decision-table));
solver-facing messages are rendered by code from stored facts and verbatim
quotes. No text is generated at runtime.

## 1. Rendering

- One placeholder exists: `{amount}`, rendered by code from
  `SolverDisputeInfo` as the fiat amount and currency (for example
  `50.000 ARS`). If the amount is unknown, the template's `_noamount` form is
  used, which is the same sentence without it.
- The first message a party receives is `intro` followed by one blank line and
  the question. `intro` is never sent again.
- The language is the party's detected language
  ([judgments.md §3](judgments.md#3-from-answers-to-facts)), or
  `default_language` until one is detected.
- The catalog lives in `messages/catalog.toml`, embedded at build time, one
  table per template id with one key per language.

## 2. Party templates

### `intro`

- **en:** Hi, I'm Serbero, an automated assistant helping the solver assigned
  to this dispute. I'll ask you and the other party a few short questions so the
  case can move faster. I cannot move funds or decide the dispute; a person can
  take over at any time, just ask.
- **es:** Hola, soy Serbero, un asistente automático que ayuda a la persona
  asignada a esta disputa. Les haré unas preguntas breves a ti y a la otra parte
  para que el caso avance más rápido. No puedo mover fondos ni decidir la
  disputa; una persona puede intervenir cuando quieras, solo pídelo.
- **pt:** Olá, sou o Serbero, um assistente automático que ajuda a pessoa
  responsável por esta disputa. Vou fazer algumas perguntas rápidas a você e à
  outra parte para agilizar o caso. Não posso mover fundos nem decidir a
  disputa; uma pessoa pode assumir a qualquer momento, é só pedir.

### `ask_buyer_sent`

- **en:** Did you send the {amount} payment for this order? If you did, please
  tell me when and from which account or app.
- **es:** ¿Enviaste el pago de {amount} de esta orden? Si lo hiciste, dime
  cuándo y desde qué cuenta o aplicación.
- **pt:** Você enviou o pagamento de {amount} deste pedido? Se sim, me diga
  quando e de qual conta ou aplicativo.

### `ask_buyer_sent_simple`

- **en:** Just to confirm: have you sent the {amount} payment? Yes or no is
  enough.
- **es:** Solo para confirmar: ¿ya enviaste el pago de {amount}? Basta con sí o
  no.
- **pt:** Só para confirmar: você já enviou o pagamento de {amount}? Basta sim
  ou não.

### `ask_buyer_details`

- **en:** Thanks. To help the solver verify it, please share any detail of the
  payment: the time, the transaction reference, the account or app you used, or
  a screenshot of the receipt with personal data covered.
- **es:** Gracias. Para que se pueda verificar, comparte algún dato del pago: la
  hora, la referencia de la operación, la cuenta o aplicación que usaste, o una
  captura del comprobante con tus datos personales tapados.
- **pt:** Obrigado. Para ajudar na verificação, compartilhe algum dado do
  pagamento: o horário, a referência da operação, a conta ou aplicativo usado,
  ou uma captura do comprovante com seus dados pessoais cobertos.

### `ask_seller_received`

- **en:** Has the {amount} payment for this order arrived in your account?
- **es:** ¿Te llegó a tu cuenta el pago de {amount} de esta orden?
- **pt:** O pagamento de {amount} deste pedido chegou na sua conta?

### `ask_seller_received_simple`

- **en:** Just to confirm: have you received the {amount} payment? Yes or no is
  enough.
- **es:** Solo para confirmar: ¿recibiste el pago de {amount}? Basta con sí o
  no.
- **pt:** Só para confirmar: você recebeu o pagamento de {amount}? Basta sim ou
  não.

### `ask_seller_check_account`

- **en:** Thanks. Could you check the account or app where you expected the
  payment, including pending movements, and tell me if you see anything for
  {amount}?
- **es:** Gracias. ¿Puedes revisar la cuenta o aplicación donde esperabas el
  pago, incluidos los movimientos pendientes, y decirme si ves algo por
  {amount}?
- **pt:** Obrigado. Pode verificar a conta ou aplicativo onde esperava o
  pagamento, incluindo movimentos pendentes, e me dizer se aparece algo de
  {amount}?

### `what_happens_next`

- **en:** I'm collecting what each of you says so the assigned solver can review
  the case quickly. Answering my question is the best way to help right now.
- **es:** Estoy reuniendo lo que dice cada uno para que la persona asignada
  pueda revisar el caso rápido. Responder mi pregunta es la mejor forma de
  ayudar ahora.
- **pt:** Estou reunindo o que cada um diz para que a pessoa responsável possa
  revisar o caso rapidamente. Responder à minha pergunta é a melhor forma de
  ajudar agora.

### `thanks_waiting`

- **en:** Thank you, that's all I need from you for now. I'll let you know if
  anything else is needed.
- **es:** Gracias, es todo lo que necesito de ti por ahora. Te aviso si hace
  falta algo más.
- **pt:** Obrigado, é tudo o que preciso de você por enquanto. Aviso se precisar
  de mais alguma coisa.

### `reminder`

- **en:** I'm still waiting for your answer to my previous question. If I don't
  hear from you soon, the case will go to the assigned solver as it is.
- **es:** Sigo esperando tu respuesta a mi pregunta anterior. Si no tengo
  noticias pronto, el caso pasará a la persona asignada tal como está.
- **pt:** Ainda estou aguardando sua resposta à minha pergunta anterior. Se não
  tiver notícias em breve, o caso seguirá para a pessoa responsável como está.

### `guide_arrived_seller`

Path `PaymentArrived`, to the seller. Sent only after the seller said the
payment arrived.

- **en:** Thanks for confirming. If you have checked that the {amount} is in
  your account, you can complete the trade yourself by releasing it from your
  Mostro app; that also closes this dispute. If anything doesn't add up, tell
  me here and a person will review it.
- **es:** Gracias por confirmarlo. Si ya verificaste que los {amount} están en
  tu cuenta, puedes completar la operación tú mismo liberándola desde tu app de
  Mostro; eso también cierra esta disputa. Si algo no cuadra, dímelo aquí y una
  persona lo revisará.
- **pt:** Obrigado por confirmar. Se você já verificou que os {amount} estão na
  sua conta, pode concluir a operação liberando-a no seu app do Mostro; isso
  também encerra esta disputa. Se algo não bater, me diga aqui e uma pessoa vai
  revisar.

### `guide_arrived_buyer`

Path `PaymentArrived`, to the buyer.

- **en:** The seller reports that your payment arrived, so they can now complete
  the trade from their Mostro app. I'll let you know when the dispute is
  closed; if something goes wrong, tell me here.
- **es:** El vendedor informa que tu pago llegó, así que ahora puede completar
  la operación desde su app de Mostro. Te aviso cuando la disputa se cierre; si
  algo sale mal, dímelo aquí.
- **pt:** O vendedor informa que seu pagamento chegou, então agora ele pode
  concluir a operação no app do Mostro. Aviso quando a disputa for encerrada; se
  algo der errado, me diga aqui.

### `guide_not_sent_buyer`

Path `PaymentNotSent`, to the buyer. Sent only after the buyer said they have
not paid.

- **en:** Thanks for being clear. Since the payment hasn't been sent, you and
  the seller can agree how to continue: you can both request a cooperative
  cancellation from the Mostro app, or, if the seller agrees, you can still
  complete the payment. If you can't agree, tell me here and a person will
  review it.
- **es:** Gracias por aclararlo. Como el pago no se envió, tú y el vendedor
  pueden acordar cómo seguir: ambos pueden solicitar una cancelación cooperativa
  desde la app de Mostro o, si el vendedor está de acuerdo, todavía puedes
  completar el pago. Si no se ponen de acuerdo, dímelo aquí y una persona lo
  revisará.
- **pt:** Obrigado por esclarecer. Como o pagamento não foi enviado, você e o
  vendedor podem combinar como seguir: ambos podem solicitar um cancelamento
  cooperativo no app do Mostro ou, se o vendedor concordar, você ainda pode
  concluir o pagamento. Se não chegarem a um acordo, me diga aqui e uma pessoa
  vai revisar.

### `guide_not_sent_seller`

Path `PaymentNotSent`, to the seller.

- **en:** The buyer reports the payment has not been sent. You can agree
  together how to continue: you can both request a cooperative cancellation
  from the Mostro app, or wait for the payment if you both agree. If you can't
  agree, tell me here and a person will review it.
- **es:** El comprador informa que el pago no se envió. Pueden acordar juntos
  cómo seguir: ambos pueden solicitar una cancelación cooperativa desde la app
  de Mostro, o esperar el pago si los dos están de acuerdo. Si no se ponen de
  acuerdo, dímelo aquí y una persona lo revisará.
- **pt:** O comprador informa que o pagamento não foi enviado. Vocês podem
  combinar como seguir: ambos podem solicitar um cancelamento cooperativo no app
  do Mostro, ou aguardar o pagamento se os dois concordarem. Se não chegarem a
  um acordo, me diga aqui e uma pessoa vai revisar.

### `resolved_thanks`

Sent to both parties when Mostro reports the dispute resolved while Serbero was
assisting.

- **en:** The dispute is now closed. Thank you both for resolving it.
- **es:** La disputa ya está cerrada. Gracias a los dos por resolverla.
- **pt:** A disputa foi encerrada. Obrigado a ambos por resolvê-la.

### `handoff_notice`

Sent to both parties on every handoff.

- **en:** Thank you both. I've passed everything you said to the assigned
  solver, who will continue with the case. They may contact you here.
- **es:** Gracias a los dos. Le pasé todo lo que dijeron a la persona asignada,
  que continuará con el caso. Puede que te escriba por aquí.
- **pt:** Obrigado a ambos. Passei tudo o que vocês disseram para a pessoa
  responsável, que continuará com o caso. Ela pode entrar em contato por aqui.

## 3. Solver messages

Sent as NIP-17 gift-wrapped DMs. They never include a party's primary pubkey,
only the trade role. Solver messages are in English.

### New dispute / reminder / taken

```text
New Mostro dispute
dispute: <dispute_id>
opened by: seller
```

```text
Dispute still unattended (32 min)
dispute: <dispute_id>
```

```text
Dispute taken
dispute: <dispute_id>
solver: <npub | Serbero>
```

### Mediation started

```text
Serbero is mediating dispute <dispute_id>.
You can take it over at any time; Serbero stops as soon as you do.
```

### Brief (on handoff or guidance)

Rendered from the last turn's facts and the brief request
([judgments.md §5](judgments.md#5-brief-request)). Every quote is verbatim.

```text
Dispute <dispute_id> · handed off: conflicting_claims
Topic: payment_not_confirmed (0.91) · rounds: 2 · duration: 14 min

Buyer — says sent (0.96), details given (0.88)
  "ya envié el pago a las 14:10 desde mi cuenta de Mercado Pago, ref 8841…"  [1 attachment]
Seller — says not received (0.93), checked account (0.90)
  "revisé el home banking y no hay ningún ingreso en pesos"

Signals: conflict 0.87 · fraud 0.08 · human requested: no
First to read: "…"  (quote_concern, when present)

Reading of the conversation (advisory, not a verdict):
  evidence that fiat was sent: 3.1 / 4  [0:0.02 1:0.05 2:0.12 3:0.49 4:0.32]

Transcript (18 messages) follows in the next message.
```

The full transcript follows as a second DM, one line per message
(`[hh:mm] buyer: …`), so the solver has everything even though they cannot
read Serbero's chat keys. When the brief is sent because of
`judge_unavailable`, the facts section is replaced by
`Automated reading unavailable` and the transcript is still sent.

### Update after handoff

```text
Dispute <dispute_id> · new messages since handoff (2)
[14:52] seller: ya me llegó, estaba pendiente
[14:53] buyer: perfecto gracias
```

### Final report

```text
Dispute <dispute_id> resolved: <status>
mediation: yes · outcome: handed_off (conflicting_claims) · rounds: 2 · duration: 41 min
```

## 4. Template rules

Enforced by tests over `messages/catalog.toml`:

1. Every template has every language in `[mediation].languages`.
2. The only placeholder is `{amount}`; every template that uses it has a
   `_noamount` form.
3. Fund-action words (`release`, `cancel`, and their translations such as
   `liberar`, `cancelación`, `cancelamento`) appear only in `guide_*`
   templates. Verdict words (`winner`, `loser`, `guilty`, `ganador`,
   `culpable`, `vencedor`, and so on) appear in no template. Adding a language
   means extending both lists in the same change.
4. `guide_*` templates describe options the party can choose in their own
   Mostro app, conditioned on what that party verified ("if you have checked
   that…"). They never tell a party that they must act, and they always offer
   a human.
5. Question templates ask about past facts ("did you send", "did it arrive").
6. Every template is short enough to read on a phone without scrolling
   (≤ 450 characters in every language).
