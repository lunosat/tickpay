# TickPay — Fake Acquirer for Load Tests

Simulador de adquirente em **Rust (Axum + Tokio)** para cenários de homologação e testes de carga. Ele **cria invoices** e, após um **delay configurável**, **emite um webhook** com o **status** desejado. Ideal para validar fluxos de checkout, retentativas e conciliações sem tocar uma adquirente real.

---

## Tabela de Conteúdos

* [Recursos](#recursos)
* [Arquitetura](#arquitetura)
* [API](#api)

  * [Criar invoice — `POST /invoices`](#criar-invoice--post-invoices)
  * [Obter invoice — `GET /invoices/:id`](#obter-invoice--get-invoicesid)
  * [Assinatura HMAC do Webhook](#assinatura-hmac-do-webhook)
* [Execução](#execução)

  * [Docker (Distroless — recomendado)](#docker-distroless--recomendado)
  * [Docker (Static musl + scratch)](#docker-static-musl--scratch)
  * [Bare-metal com systemd](#bare-metal-com-systemd)
* [Configuração](#configuração)
* [Exemplos Rápidos](#exemplos-rápidos)
* [Testes de Carga](#testes-de-carga)
* [Boas Práticas de Produção](#boas-práticas-de-produção)
* [Resolução de Problemas](#resolução-de-problemas)
* [Licença](#licença)

---

## Recursos

* **Invoices temporizadas**: define `emit_after_ms` e `emit_status` no momento da criação.
* **Webhook dinâmico**: envia para o `webhook_url` informado na requisição.
* **HMAC-SHA256**: assinatura em `X-Signature` usando `ACQ_WEBHOOK_SECRET`.
* **Idempotência** (opcional): respeita header `Idempotency-Key`.
* **CORS + tracing**: úteis para debug.

> **Status suportados**: `paid`, `failed`, `canceled`, `expired`, `chargeback`.

---

## Arquitetura

* **Axum 0.7** para HTTP server.
* **Tokio** agenda a tarefa que aguarda o delay e envia o webhook.
* **DashMap** em memória (sem persistência; reinício limpa tudo). Substitua por um DB se precisar.
* **reqwest + rustls** com CAs embutidas (`webpki-roots`) para rodar em imagens mínimas.

---

## API

### Criar invoice — `POST /invoices`

**Request headers**

* `Content-Type: application/json`
* `Idempotency-Key: <string>` *(opcional — evita duplicações do mesmo pedido)*

**Request body**

```json
{
  "amount": 10000,
  "currency": "BRL",
  "webhook_url": "https://seu-receiver.tld/webhook",
  "emit_after_ms": 5000,
  "emit_status": "paid",
  "metadata": { "order_id": "ORD-123" }
}
```

**Campos**

* `amount` *(u64, obrigatório)* — em centavos.
* `currency` *(string, opcional — default `BRL`)*.
* `webhook_url` *(string, obrigatório)* — `http` ou `https`.
* `emit_after_ms` *(u64, opcional — default `5000`)* — delay em ms.
* `emit_status` *(enum, obrigatório)* — `paid|failed|canceled|expired|chargeback`.
* `metadata` *(obj, opcional)* — ecoado na resposta e no webhook.

**Response 201**

```json
{
  "id": "c0b3c2c8-6a5f-4c61-9c21-7a5e0a4c2e75",
  "status": "created",
  "amount": 10000,
  "currency": "BRL",
  "created_at": "2025-10-22T17:00:00Z",
  "webhook_url": "https://seu-receiver.tld/webhook",
  "checkout_url": "https://checkout.local/invoice/c0b3c2c8-6a5f-4c61-9c21-7a5e0a4c2e75",
  "metadata": { "order_id": "ORD-123" }
}
```

> Após `emit_after_ms`, o serviço atualiza o status em memória e **POSTa** o webhook.

### Obter invoice — `GET /invoices/:id`

**Response 200**

```json
{
  "id": "c0b3c2c8-6a5f-4c61-9c21-7a5e0a4c2e75",
  "amount": 10000,
  "currency": "BRL",
  "status": "paid",
  "webhook_url": "https://seu-receiver.tld/webhook",
  "created_at": "2025-10-22T17:00:00Z",
  "metadata": { "order_id": "ORD-123" }
}
```

### Assinatura HMAC do Webhook

* Header: `X-Signature: hex(hmac_sha256(raw_body, ACQ_WEBHOOK_SECRET))`
* Header adicional: `X-Event: invoice.updated`

**Webhook body**

```json
{
  "event": "invoice.updated",
  "id": "c0b3c2c8-6a5f-4c61-9c21-7a5e0a4c2e75",
  "status": "paid",
  "amount": 10000,
  "currency": "BRL",
  "emitted_at": "2025-10-22T17:00:05Z",
  "metadata": { "order_id": "ORD-123" }
}
```

**Exemplo de verificação (Node/Express)**

```js
import express from 'express'
import crypto from 'crypto'
const app = express()
app.use(express.json({ type: '*/*' }))
const SECRET = process.env.ACQ_WEBHOOK_SECRET || 'dev_secret'
app.post('/webhook', (req, res) => {
  const sig = req.get('X-Signature') || ''
  const raw = JSON.stringify(req.body)
  const digest = crypto.createHmac('sha256', SECRET).update(raw).digest('hex')
  const ok = Buffer.from(digest).equals(Buffer.from(sig))
  console.log({ ok, headers: req.headers, body: req.body })
  res.sendStatus(204)
})
app.listen(4000)
```

---

## Execução

### Docker (Distroless — recomendado)

* Imagem base mínima e sem pacote gestor → **menos CVEs**.
* CA roots embutidas via `rustls-tls-webpki-roots` (no `Cargo.toml`).

**Dockerfile** (ver pasta do projeto para o arquivo completo):

```dockerfile
FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder /app/target/release/fake-acquirer /usr/local/bin/fake-acquirer
ENV RUST_LOG=info ACQ_WEBHOOK_SECRET=change_me PORT=8080
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/fake-acquirer"]
```

**docker-compose.yml** (hardened):

```yaml
services:
  tickpay:
    build: .
    environment:
      - RUST_LOG=info
      - ACQ_WEBHOOK_SECRET=${ACQ_WEBHOOK_SECRET:-change_me}
      - PORT=8080
    ports:
      - "8080:8080"
    restart: unless-stopped
    security_opt:
      - no-new-privileges:true
    read_only: true
    tmpfs:
      - /tmp
    cap_drop:
      - ALL
```

Subir:

```bash
docker compose up -d --build
```

### Docker (Static musl + scratch)

* Binário estático e imagem **scratch** ultra-compacta.
* Também já disponível no projeto.

### Bare-metal com systemd

1. `cargo build --release`
2. Copie o binário para `/opt/tickpay/` e crie serviço `tickpay.service`.
3. `sudo systemctl enable --now tickpay`.

Exemplo de serviço já incluso no guia do projeto.

---

## Configuração

Variáveis de ambiente:

* `PORT` *(default `8080`)* — porta HTTP.
* `ACQ_WEBHOOK_SECRET` *(default `dev_secret`)* — segredo da HMAC.
* `RUST_LOG` *(default `info`)* — nível de log.

---

## Exemplos Rápidos

**Criar invoice**

```bash
curl -sS -X POST http://localhost:8080/invoices \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: test-123' \
  -d '{
    "amount": 10000,
    "currency": "BRL",
    "webhook_url": "http://localhost:4000/webhook",
    "emit_after_ms": 5000,
    "emit_status": "paid",
    "metadata": {"order_id": "ORD-123"}
  }' | jq .
```

**Consultar invoice**

```bash
curl -sS http://localhost:8080/invoices/<uuid> | jq .
```

---

## Testes de Carga

* **bombardier**

  ```bash
  bombardier -c 200 -n 20000 -m POST \
    -H 'Content-Type: application/json' \
    -f payload.json \
    http://localhost:8080/invoices
  ```
* **k6** (exemplo minimal)

  ```js
  import http from 'k6/http'
  import { check, sleep } from 'k6'
  export default function () {
    const res = http.post('http://localhost:8080/invoices', JSON.stringify({
      amount: 2000,
      currency: 'BRL',
      webhook_url: 'http://localhost:4000/webhook',
      emit_after_ms: 2000,
      emit_status: 'paid',
      metadata: { run: __ITER }
    }), { headers: { 'Content-Type': 'application/json' }})
    check(res, { '201': r => r.status === 201 })
    sleep(0.1)
  }
  ```

> Dica: rode múltiplas réplicas atrás do Nginx/Caddy para elevar QPS.

---

## Boas Práticas de Produção

* **Imagens mínimas**: Distroless ou scratch → menos CVEs.
* **Hardening**: usuário não-root, FS somente leitura, `cap_drop: ALL`, `no-new-privileges`.
* **Firewall**: exponha apenas via proxy; app escutando em `localhost`.
* **Observabilidade**: ajuste `RUST_LOG` para `debug` em ambientes de teste.
* **Idempotência**: use `Idempotency-Key` nos seus clientes para evitar duplicatas.


# tickpay
