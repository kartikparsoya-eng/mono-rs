-- Minimal xyne-like schema. Column names use camelCase (quoted) to
-- match what xyne's Zero schema expects so captured ASTs from xyne
-- can be transplanted with minimal rewriting.

-- Note: this file lives in its own database now; no schema namespacing.
-- schema defaulted to public


CREATE TABLE channels (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  visibility  TEXT NOT NULL  -- 'public' | 'private'
);

CREATE TABLE users (
  id    TEXT PRIMARY KEY,
  name  TEXT NOT NULL
);

CREATE TABLE participants (
  "userId"     TEXT NOT NULL REFERENCES users(id),
  "channelId"  TEXT NOT NULL REFERENCES channels(id),
  PRIMARY KEY ("userId", "channelId")
);

CREATE TABLE conversations (
  id           TEXT PRIMARY KEY,
  "channelId"  TEXT NOT NULL REFERENCES channels(id),
  title        TEXT NOT NULL,
  "createdAt"  BIGINT NOT NULL
);

CREATE TABLE messages (
  id               TEXT PRIMARY KEY,
  "conversationId" TEXT NOT NULL REFERENCES conversations(id),
  "authorId"       TEXT NOT NULL REFERENCES users(id),
  body             TEXT NOT NULL,
  "createdAt"      BIGINT NOT NULL,
  "visibleTo"      TEXT NULL
);

CREATE TABLE attachments (
  id               TEXT PRIMARY KEY,
  "messageId"      TEXT NOT NULL REFERENCES messages(id),
  "conversationId" TEXT NOT NULL REFERENCES conversations(id),
  filename         TEXT NOT NULL,
  "createdAt"      BIGINT NOT NULL
);

CREATE INDEX messages_conversation_idx ON messages("conversationId");
CREATE INDEX attachments_conversation_idx ON attachments("conversationId");
CREATE INDEX participants_channel_idx ON participants("channelId");

-- Compound-key tables for compound join key coverage.
CREATE TABLE departments (
  "orgID"   TEXT NOT NULL,
  "deptID"  TEXT NOT NULL,
  name      TEXT,
  PRIMARY KEY ("orgID", "deptID")
);

CREATE TABLE team_members (
  id       TEXT PRIMARY KEY,
  "orgID"  TEXT NOT NULL,
  "deptID" TEXT NOT NULL,
  name     TEXT
);

CREATE INDEX team_members_org_dept_idx ON team_members("orgID", "deptID");

-- =============================================================================
-- FUZZ-02 schema extension per CONTEXT.md D-09..D-12.
-- Production-shape density (2x JSONB, 2x TIMESTAMPTZ on events) per D-09;
-- target apps/zbugs/shared/schema.ts. Adds NUMERIC, BIGINT, GIN indexes,
-- and a composite-PK + JSONB child for partition-Take coverage.
-- =============================================================================

-- events: production-shape rich-type table. Multiple JSONB and TIMESTAMPTZ
-- columns exercise both type-coercion paths (the second one catches silent
-- divergences the first one's stale value would mask).
-- relatedTicketId references conversations(id) — the xyne-style schema has
-- no `tickets` table; column name kept for plan-traceability.
CREATE TABLE events (
  id                TEXT PRIMARY KEY,
  "occurredAt"      TIMESTAMPTZ NOT NULL,
  "processedAt"     TIMESTAMPTZ NULL,
  "metadataJson"    JSONB NOT NULL DEFAULT '{}'::JSONB,
  "auditTrail"      JSONB NULL,
  amount            NUMERIC(20,6) NOT NULL DEFAULT 0,
  quantity          BIGINT NOT NULL,
  "isProcessed"     BOOLEAN NOT NULL DEFAULT false,
  "actorUserId"     TEXT NULL REFERENCES users(id),
  notes             TEXT NULL,
  "relatedTicketId" TEXT NULL REFERENCES conversations(id)
);
CREATE INDEX events_occurredAt_idx     ON events ("occurredAt" DESC);
CREATE INDEX events_processedAt_idx    ON events ("processedAt");
CREATE INDEX events_actorUserId_idx    ON events ("actorUserId");
CREATE INDEX events_metadataJson_gin   ON events USING GIN ("metadataJson");
CREATE INDEX events_auditTrail_gin     ON events USING GIN ("auditTrail");

-- big_id_records: i64 > 2^53 boundary surface for B8/B9 organic discovery
-- per D-12. IDs stored as TEXT but seeded with values just below, at, and
-- above 2^53 so f64-coercion bugs surface immediately.
CREATE TABLE big_id_records (
  id              TEXT PRIMARY KEY,
  label           TEXT NOT NULL,
  "parentBigId"   TEXT NULL,
  "createdAt"     TIMESTAMPTZ NOT NULL
);
CREATE INDEX big_id_records_parentBigId_idx ON big_id_records ("parentBigId");

-- event_tags: composite PK (eventId, tagKey) + JSONB for partition-Take
-- coverage with rich type. confidence is NUMERIC(5,4) so 0..1 values
-- preserve precision through the wire protocol.
CREATE TABLE event_tags (
  "eventId"       TEXT NOT NULL REFERENCES events(id),
  "tagKey"        TEXT NOT NULL,
  "tagValue"      TEXT NOT NULL,
  confidence      NUMERIC(5,4) NOT NULL,
  "metadataJson"  JSONB NOT NULL DEFAULT '{}'::JSONB,
  PRIMARY KEY ("eventId", "tagKey")
);
CREATE INDEX event_tags_eventId_idx ON event_tags ("eventId");
