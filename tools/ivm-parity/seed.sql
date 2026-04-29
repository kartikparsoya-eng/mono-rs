

-- ~30 rows of synthetic data, chosen so every test query has a
-- non-trivial result set (non-zero, non-trivial filtering).

-- 3 channels: 2 public, 1 private.
INSERT INTO channels (id, name, visibility) VALUES
  ('ch-pub-1',  'general',     'public'),
  ('ch-pub-2',  'random',      'public'),
  ('ch-priv-1', 'eng-private', 'private');

-- 3 users.
INSERT INTO users (id, name) VALUES
  ('u1', 'alice'),
  ('u2', 'bob'),
  ('u3', 'carol');

-- u1 is in both public channels + the private one.
-- u2 is in public channels only.
-- u3 is in the private channel only.
INSERT INTO participants ("userId", "channelId") VALUES
  ('u1', 'ch-pub-1'),
  ('u1', 'ch-pub-2'),
  ('u1', 'ch-priv-1'),
  ('u2', 'ch-pub-1'),
  ('u2', 'ch-pub-2'),
  ('u3', 'ch-priv-1');

-- 5 conversations spread across channels.
INSERT INTO conversations (id, "channelId", title, "createdAt") VALUES
  ('co-1', 'ch-pub-1',  'welcome',        1000),
  ('co-2', 'ch-pub-1',  'standup',        2000),
  ('co-3', 'ch-pub-2',  'watercooler',    3000),
  ('co-4', 'ch-priv-1', 'secret project', 4000),
  ('co-5', 'ch-priv-1', 'layoffs',        5000);

-- 10 messages, some with attachments, some visibility-restricted.
INSERT INTO messages (id, "conversationId", "authorId", body, "createdAt", "visibleTo") VALUES
  ('m-1',  'co-1', 'u1', 'hi team',        1100, NULL),
  ('m-2',  'co-1', 'u2', 'welcome',        1200, NULL),
  ('m-3',  'co-2', 'u1', 'standup notes',  2100, NULL),
  ('m-4',  'co-2', 'u2', 'blocked on db',  2200, NULL),
  ('m-5',  'co-3', 'u1', 'coffee anyone?', 3100, NULL),
  ('m-6',  'co-4', 'u1', 'design doc',     4100, NULL),
  ('m-7',  'co-4', 'u3', 'ship date',      4200, NULL),
  ('m-8',  'co-5', 'u1', 'announcement',   5100, NULL),
  -- visibility-restricted: only u3 can see.
  ('m-9',  'co-5', 'u1', 'priv msg u3',    5200, 'u3'),
  -- visibility-restricted: only u1 can see.
  ('m-10', 'co-1', 'u2', 'priv msg u1',    1300, 'u1');

-- 6 attachments across messages.
INSERT INTO attachments (id, "messageId", "conversationId", filename, "createdAt") VALUES
  ('a-1', 'm-1', 'co-1', 'welcome.png',    1101),
  ('a-2', 'm-3', 'co-2', 'standup.pdf',    2101),
  ('a-3', 'm-6', 'co-4', 'design-v1.pdf',  4101),
  ('a-4', 'm-6', 'co-4', 'design-v2.pdf',  4102),
  ('a-5', 'm-7', 'co-4', 'gantt.png',      4201),
  ('a-6', 'm-8', 'co-5', 'roadmap.pdf',    5101);

-- 3 departments across 2 orgs (compound PK: orgID + deptID).
INSERT INTO departments ("orgID", "deptID", name) VALUES
  ('acme',   'eng',   'Engineering'),
  ('acme',   'sales', 'Sales'),
  ('globex', 'eng',   'Engineering');

-- 4 team members joining to departments on (orgID, deptID).
INSERT INTO team_members (id, "orgID", "deptID", name) VALUES
  ('tm1', 'acme',   'eng',   'Alice'),
  ('tm2', 'acme',   'eng',   'Bob'),
  ('tm3', 'acme',   'sales', 'Carol'),
  ('tm4', 'globex', 'eng',   'Dave');

-- ============================================================
-- Prod-extension seed data.
-- Mix of nullable / non-null / archived / orphan-FK rows so
-- WHERE+ORDER queries return non-empty diverse results.
-- ============================================================

-- 6 tickets — covers: assignedTo NULL, archived, multi-channel, multi-creator
INSERT INTO tickets
  (id,    title,            status,   "ticketType", "createdBy", "createdAt",
   "conversationId", "assignedTo", "channelId", "boardId", "projectId",
   "isArchived", "lastEmailAt") VALUES
  ('t-1', 'login bug',      'open',   'bug',        'u1',        10100,
   'co-1', 'u2', 'ch-pub-1', 'b-1', 'p-1', false, 10500),
  ('t-2', 'feature req',    'open',   'feature',    'u1',        10200,
   NULL,   NULL, 'ch-pub-1', 'b-1', 'p-1', false, NULL),
  ('t-3', 'docs typo',      'closed', 'task',       'u2',        10300,
   'co-2', 'u1', 'ch-pub-1', 'b-2', 'p-1', false, 10700),
  ('t-4', 'archived item',  'closed', 'bug',        'u1',        10400,
   NULL,   'u3', 'ch-priv-1', 'b-2', 'p-2', true,  NULL),
  ('t-5', 'no-channel',     'open',   'task',       'u3',        10500,
   NULL,   NULL, NULL, 'b-3', 'p-2', false, 10900),
  ('t-6', 'priv board',     'open',   'feature',    'u3',        10600,
   'co-4', 'u1', 'ch-priv-1', 'b-3', NULL, false, 11000);

-- 5 activities — read/unread, with/without messageId
INSERT INTO activities
  (id, "userId", "actorAction", "actionSource", "isRead",
   "messageId", classification, "updatedAt") VALUES
  ('act-1', 'u1', 'mention',  'message',  false, 'm-3', 'high', 20100),
  ('act-2', 'u2', 'reply',    'message',  true,  'm-4', 'high', 20200),
  ('act-3', 'u1', 'reaction', 'message',  false, 'm-5', NULL,   20300),
  ('act-4', 'u3', 'invite',   'channel',  false, NULL,  'low',  20400),
  ('act-5', 'u1', 'mention',  'thread',   true,  'm-6', NULL,   20500);

-- 5 canvases — different docTypes + access patterns
INSERT INTO canvases
  (id,    title,        "channelId", "createdBy", "viewAccessId",
   "editAccessId", "docType", "userRepo", "updatedAt") VALUES
  ('cv-1', 'design v1',  'ch-pub-1', 'u1', 'view-public', 'edit-team', 'doc',  NULL,        30100),
  ('cv-2', 'roadmap',    'ch-pub-1', 'u1', 'view-team',   'edit-u1',   'doc',  NULL,        30200),
  ('cv-3', 'private',    'ch-priv-1','u3', 'view-priv',    NULL,       'sheet','repo-priv', 30300),
  ('cv-4', 'orphan-cv',  NULL,        'u2', NULL,          NULL,       'doc',   'repo-x',   30400),
  ('cv-5', 'shared',     'ch-pub-2', 'u1', 'view-public', 'edit-public','board','repo-y',   30500);

-- 6 channel_recaps — multiple per channel, with/without userId
INSERT INTO channel_recaps
  (id,     "channelId", "recapDate", summary,           "userId") VALUES
  ('cr-1', 'ch-pub-1',  20260401,    'good week',       'u1'),
  ('cr-2', 'ch-pub-1',  20260402,    'busy day',         NULL),
  ('cr-3', 'ch-pub-2',  20260401,    'water cooler',     'u2'),
  ('cr-4', 'ch-priv-1', 20260401,    'project status',  'u3'),
  ('cr-5', 'ch-priv-1', 20260402,    'next steps',       NULL),
  ('cr-6', 'ch-pub-1',  20260403,    'weekend recap',   'u1');

-- 5 calls — running/scheduled/ended, mixed channelId
INSERT INTO calls
  (id,     title,       "callType", status,      "channelId", "startsAt", "startedAt") VALUES
  ('cl-1', 'standup',   'video',    'ended',     'ch-pub-1',  40100, 40110),
  ('cl-2', 'planning',  'video',    'live',      'ch-pub-1',  40200, 40205),
  ('cl-3', '1:1',       'audio',    'scheduled', 'ch-pub-2',  40300, NULL),
  ('cl-4', 'eng sync',  'video',    'ended',     'ch-priv-1', 40400, 40402),
  ('cl-5', 'no-channel','video',    'scheduled', NULL,        40500, NULL);
