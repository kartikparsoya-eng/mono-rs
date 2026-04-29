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

-- ============================================================================
-- FUZZ-02 (Phase 34) — production-shape fixtures.
-- CONTEXT D-11/D-12. Strictly additive (SKILL.md hard rule #2). New IDs use
-- prefixes ev- (events), eb- (big_id_records), et- (event_tags) so they're
-- easy to grep and never collide with the base seed.
-- ============================================================================

-- events: NULL semantics + jsonb + timestamptz + numeric coverage.
-- D-11: NULL ≠ NULL in equality (ev-null-1 vs ev-null-2 both with NULL FKs);
--       NULL in OR three-valued logic (ev-or-mixed);
--       NULL in JOIN keys (ev-no-actor — actorUserId is NULL).
INSERT INTO events
  (id, "occurredAt", "metadataJson", amount, quantity, "isProcessed",
   "actorUserId", notes, "relatedTicketId")
VALUES
  ('ev-null-1', '2026-04-01T00:00:00Z', '{}', 0,    0,   false, NULL,  NULL,         NULL),
  ('ev-null-2', '2026-04-01T01:00:00Z', '{}', 0,    0,   false, NULL,  NULL,         NULL),
  ('ev-or-mixed', '2026-04-02T00:00:00Z', '{"k":"v"}', 1.500000, 100, true,
   'u1',  'note-with-value',  't-1'),
  ('ev-no-actor', '2026-04-03T00:00:00Z', '{}', 2.000000, 200, false,
   NULL,  'pure null actor',   NULL),
  -- DST round-trips (CONTEXT D-12). US Eastern 2026: spring 03-08 02→03,
  -- fall 11-01 02→01.
  ('ev-dst-spring', '2026-03-08T06:30:00Z', '{}', 0, 0, false, 'u1', 'spring forward', NULL),
  ('ev-dst-fall',   '2026-11-01T05:30:00Z', '{}', 0, 0, false, 'u1', 'fall back', NULL),
  -- jsonb fixtures (literal-equality vs whitespace-canonical-equality fuzz).
  ('ev-jsonb-rich',
   '2026-04-04T00:00:00Z',
   '{"priority": "high", "tags": ["a", "b"]}',
   0.000000, 0, false, 'u1', NULL, NULL),
  ('ev-jsonb-empty', '2026-04-05T00:00:00Z', '{}', 0, 0, false, 'u2', NULL, NULL);

-- big_id_records: i64 > 2^53 boundary fixtures (B8/B9 organic surface).
-- 2^53 = 9007199254740992. Use values just below, at, above, far above.
-- parentBigId chains create a one-table self-join exercising NULL ≠ NULL.
INSERT INTO big_id_records (id, label, "parentBigId", "createdAt") VALUES
  ('9007199254740991', 'safe-int-max',          NULL,                '2026-04-01T00:00:00Z'),
  ('9007199254740992', '2^53-exact',            '9007199254740991',  '2026-04-01T01:00:00Z'),
  ('9007199254740993', 'first-unsafe',          '9007199254740992',  '2026-04-01T02:00:00Z'),
  ('9007199254740994', 'second-unsafe',         '9007199254740993',  '2026-04-01T03:00:00Z'),
  ('9999999999999999', 'far-unsafe',            '9007199254740994',  '2026-04-01T04:00:00Z'),
  -- Two NULL-parent rows so NULL ≠ NULL in JOIN keys is testable.
  ('eb-orphan-1',      'orphan-no-parent',      NULL,                '2026-04-01T05:00:00Z'),
  ('eb-orphan-2',      'another-orphan',        NULL,                '2026-04-01T06:00:00Z');

-- event_tags: compound-key partition-Take coverage. Wave 0 supplies enough
-- rows that a Take(limit=2) on the per-event partition has a meaningful window.
INSERT INTO event_tags ("eventId", "tagKey", "tagValue", confidence, "metadataJson") VALUES
  ('ev-or-mixed',   'priority',  'high',   0.9500, '{}'),
  ('ev-or-mixed',   'category',  'bug',    0.8500, '{}'),
  ('ev-or-mixed',   'severity',  'p1',     0.9000, '{}'),
  ('ev-jsonb-rich', 'priority',  'low',    0.5000, '{}'),
  ('ev-jsonb-rich', 'category',  'task',   0.7500, '{}'),
  ('ev-jsonb-empty','priority',  'medium', 0.6000, '{}');
