-- Additive seed extension. Layered on top of `seed.sql` for richer
-- coverage: edge-case timestamps, ILIKE-friendly text, multiple visibility
-- variants, deeper Take/Skip cursor surface area, NULL pile-ups in nullable
-- columns. Apply AFTER seed.sql:
--
--   psql $XYNE_LITE_PG_URL -f seed.sql
--   psql $XYNE_LITE_PG_URL -f seed-extras.sql
--
-- Strictly additive — never UPDATE or DELETE existing rows. IDs are
-- prefixed `x-` (extras) so they're easy to grep and never collide with
-- the base seed.

-- 2 more channels — `x-ch-empty` has zero conversations (matters for
-- NOT EXISTS branches); `x-ch-deep` has many conversations to exercise
-- Take/Skip / refetch.
INSERT INTO channels (id, name, visibility) VALUES
  ('x-ch-empty', 'empty-channel',           'public'),
  ('x-ch-deep',  'deep-history',            'public');

-- 2 more users — long names for ILIKE windowing.
INSERT INTO users (id, name) VALUES
  ('x-u-long', 'alice-anderson-engineering'),
  ('x-u-edge', 'edge-case-user');

-- u1/u2 join the deep-history channel; nobody joins the empty one.
INSERT INTO participants ("userId", "channelId") VALUES
  ('u1',       'x-ch-deep'),
  ('u2',       'x-ch-deep'),
  ('x-u-edge', 'x-ch-deep');

-- 5 conversations on x-ch-deep at varied timestamps so cursor + limit
-- branches see a real Take window. Titles include searchable text.
INSERT INTO conversations (id, "channelId", title, "createdAt") VALUES
  ('x-co-1', 'x-ch-deep',  'standup-monday',     6000),
  ('x-co-2', 'x-ch-deep',  'planning-session',   6500),
  ('x-co-3', 'x-ch-deep',  'standup-tuesday',    7000),
  ('x-co-4', 'x-ch-deep',  'retrospective',      7500),
  ('x-co-5', 'x-ch-deep',  'standup-wednesday',  8000);

-- 8 messages with varied visibility — gives nullable visibleTo column
-- both NULL and non-NULL ranges large enough that NOT IN [], IS NOT NULL,
-- and IN [...] discriminate a meaningful subset.
INSERT INTO messages (id, "conversationId", "authorId", body, "createdAt", "visibleTo") VALUES
  ('x-m-1',  'x-co-1', 'u1',       'standup notes for monday',     6100, NULL),
  ('x-m-2',  'x-co-1', 'u2',       'reviewed the plan',            6200, NULL),
  ('x-m-3',  'x-co-2', 'u1',       'standup planning kickoff',     6600, NULL),
  ('x-m-4',  'x-co-3', 'u1',       'standup notes for tuesday',    7100, 'u1'),
  ('x-m-5',  'x-co-3', 'x-u-edge', 'edge-case message body',       7150, NULL),
  ('x-m-6',  'x-co-4', 'u1',       'retro action items',           7600, NULL),
  ('x-m-7',  'x-co-5', 'u1',       'standup notes for wednesday',  8100, NULL),
  ('x-m-8',  'x-co-5', 'u2',       'private follow-up',            8200, 'x-u-edge');

-- 4 attachments — multiple per message tests Join multi-child paths.
INSERT INTO attachments (id, "messageId", "conversationId", filename, "createdAt") VALUES
  ('x-a-1', 'x-m-1', 'x-co-1', 'monday-standup.pdf',  6101),
  ('x-a-2', 'x-m-1', 'x-co-1', 'monday-charts.png',   6102),
  ('x-a-3', 'x-m-3', 'x-co-2', 'planning-doc.pdf',    6601),
  ('x-a-4', 'x-m-7', 'x-co-5', 'wednesday-recap.pdf', 8101);

-- =============================================================================
-- FUZZ-02 fixtures per CONTEXT D-10..D-12. Additive only per SKILL.md hard
-- rule 2. -test- infix marks lifecycle as additive-test (Phase 34 namespace
-- convention) — prevents collision with future seed.sql additions and lets
-- harnesses spot which rows are safe to UPDATE/DELETE during a test cycle.
-- =============================================================================

-- D-11 NULL semantics: two rows with all-NULL FKs/notes/processedAt/
-- auditTrail. NULL ≠ NULL in equi-join — the two rows must NOT match
-- each other on actorUserId. Exercises both new D-09 NULL columns.
INSERT INTO events (id, "occurredAt", "processedAt", "metadataJson", "auditTrail",
                    amount, quantity, "isProcessed", "actorUserId", notes,
                    "relatedTicketId") VALUES
  ('ev-null-test-1',     '2026-04-01T00:00:00Z', NULL, '{}', NULL,
   0, 0, false, NULL, NULL, NULL),
  ('ev-null-test-2',     '2026-04-01T01:00:00Z', NULL, '{}', NULL,
   0, 0, false, NULL, NULL, NULL),
  ('ev-no-ticket-test',  '2026-04-03T00:00:00Z', NULL, '{}', NULL,
   2.0, 200, false, 'u2', NULL, NULL),
  -- ev-pure-or-test-1: simple-pred-matching row with non-NULL FKs and
  -- non-NULL processedAt + auditTrail (exercises both D-09 prod-shape
  -- columns in their populated state).
  ('ev-pure-or-test-1',  '2026-04-02T00:00:00Z', '2026-04-02T00:30:00Z',
   '{"k":"v"}', '{"reviewer":"u3","decision":"approved"}',
   1.5, 100, true, 'u1', 'note', 'co-1');

-- D-12 i64 > 2^53 boundary: 2^53 = 9007199254740992. Values just below,
-- at, just above, and far above. parentBigId chained on most rows; first
-- row has parentBigId=NULL for self-FK NULL semantics. f64 coercion in
-- Rust compare_values (audit B8/B9) loses precision at the boundary.
INSERT INTO big_id_records (id, label, "parentBigId", "createdAt") VALUES
  ('9007199254740991', 'safe-int-max',    NULL,                '2026-04-01T00:00:00Z'),
  ('9007199254740992', '2^53-exact',      '9007199254740991',  '2026-04-01T01:00:00Z'),
  ('9007199254740993', 'first-unsafe',    '9007199254740992',  '2026-04-01T02:00:00Z'),
  ('9007199254740994', 'second-unsafe',   '9007199254740993',  '2026-04-01T03:00:00Z'),
  ('9999999999999999', 'far-unsafe',      '9007199254740994',  '2026-04-01T04:00:00Z');

-- D-12 DST round-trip: US Eastern 2026 spring-forward (2026-03-08 02:00 →
-- 03:00) and fall-back (2026-11-01 02:00 → 01:00). UTC values chosen so
-- local-time conversion lands inside the gap / fold. Both processedAt and
-- occurredAt set to exercise both timestamptz columns through the
-- DST-affected window.
INSERT INTO events (id, "occurredAt", "processedAt", "metadataJson", "auditTrail",
                    amount, quantity, "isProcessed", "actorUserId", notes,
                    "relatedTicketId") VALUES
  ('ev-dst-spring-test', '2026-03-08T06:30:00Z', '2026-03-08T07:30:00Z',
   '{}', NULL, 0, 0, false, 'u1', 'spring forward', NULL),
  ('ev-dst-fall-test',   '2026-11-01T05:30:00Z', '2026-11-01T06:30:00Z',
   '{}', NULL, 0, 0, false, 'u1', 'fall back', NULL);

-- D-10/D-12 jsonb fixtures: non-trivial values exercise predicate paths
-- on jsonb (silent-coercion surface per pitfall 8). The non-trivial
-- auditTrail value populates the second jsonb column per D-09.
INSERT INTO events (id, "occurredAt", "processedAt", "metadataJson", "auditTrail",
                    amount, quantity, "isProcessed", "actorUserId", notes,
                    "relatedTicketId") VALUES
  ('ev-jsonb-test-1',     '2026-04-04T00:00:00Z', '2026-04-04T00:15:00Z',
   '{"priority": "high", "tags": ["a", "b"]}',
   '{"by":"u1","at":"2026-04-29T00:00:00Z"}',
   0, 0, false, 'u1', NULL, NULL),
  ('ev-jsonb-empty-test', '2026-04-05T00:00:00Z', NULL,
   '{}', NULL,
   0, 0, false, 'u2', NULL, NULL);

-- D-09 compound-PK + jsonb child rows (event_tags). Bound to two parent
-- events (ev-pure-or-test-1 and ev-jsonb-test-1) with mixed confidence
-- values for partition-Take coverage. confidence as NUMERIC(5,4) tests
-- numeric precision across the wire.
INSERT INTO event_tags ("eventId", "tagKey", "tagValue", confidence,
                        "metadataJson") VALUES
  ('ev-pure-or-test-1', 'priority', 'high', 0.9500, '{}'),
  ('ev-pure-or-test-1', 'category', 'bug',  0.8500, '{}'),
  ('ev-jsonb-test-1',   'priority', 'low',  0.5000, '{}'),
  ('ev-jsonb-test-1',   'category', 'task', 0.7500, '{}');
