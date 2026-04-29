/**
 * Zero schema for the ivm-parity harness. Table + column names mirror
 * the source app's shared schema so ASTs transplanted from the source app's queries.ts
 * (after swapping literal IDs for our seed's IDs) run unchanged.
 */
import {
  definePermissions,
  createSchema,
  string,
  number,
  boolean,
  table,
  relationships,
  ANYONE_CAN_DO_ANYTHING,
} from '@rocicorp/zero';

const channel = table('channels')
  .columns({
    id: string(),
    name: string(),
    visibility: string(),
  })
  .primaryKey('id');

const user = table('users')
  .columns({
    id: string(),
    name: string(),
  })
  .primaryKey('id');

const participant = table('participants')
  .columns({
    userId: string(),
    channelId: string(),
  })
  .primaryKey('userId', 'channelId');

const conversation = table('conversations')
  .columns({
    id: string(),
    channelId: string(),
    title: string(),
    createdAt: number(),
  })
  .primaryKey('id');

const message = table('messages')
  .columns({
    id: string(),
    conversationId: string(),
    authorId: string(),
    body: string(),
    createdAt: number(),
    visibleTo: string().optional(),
  })
  .primaryKey('id');

const attachment = table('attachments')
  .columns({
    id: string(),
    messageId: string(),
    conversationId: string(),
    filename: string(),
    createdAt: number(),
  })
  .primaryKey('id');

const department = table('departments')
  .columns({
    orgID: string(),
    deptID: string(),
    name: string().optional(),
  })
  .primaryKey('orgID', 'deptID');

const teamMember = table('team_members')
  .columns({
    id: string(),
    orgID: string(),
    deptID: string(),
    name: string().optional(),
  })
  .primaryKey('id');

// FUZZ-02 schema extension per CONTEXT.md D-09..D-12; production-shape density
// (2x jsonb, 2x timestamptz on events) per D-09; target apps/zbugs/shared/schema.ts.
// Zero replicates JSONB/TIMESTAMPTZ/NUMERIC/BIGINT as `string()` — the rich PG
// type lives in schema.sql; the AST-level fuzzer sees these as text columns.
// See .planning/phases/34-differential-fuzz-schema-extension/34-RESEARCH.md
// (Schema Extension Layout) for the full rationale and Assumption A1 on
// BIGINT replication.

const event = table('events')
  .columns({
    id: string(),
    occurredAt: string(), // PG TIMESTAMPTZ — replicated as ISO string
    processedAt: string().optional(), // PG TIMESTAMPTZ NULL — D-09 prod-shape jsonb/ts #2
    metadataJson: string(), // PG JSONB — replicated as JSON string
    auditTrail: string().optional(), // PG JSONB NULL — D-09 prod-shape jsonb #2
    amount: string(), // PG NUMERIC — replicated as string (preserves precision)
    quantity: number(), // PG BIGINT — Zero replicates >2^53 as string per A1; here number for fuzz
    isProcessed: boolean(),
    actorUserId: string().optional(),
    notes: string().optional(), // nullable text — NULL semantics fixtures (D-11)
    relatedTicketId: string().optional(), // FK→conversations(id); xyne-style schema has no tickets table
  })
  .primaryKey('id');

const bigIdRecord = table('big_id_records')
  .columns({
    id: string(), // PG TEXT (string-encoded BIGINT >2^53 for B8/B9 organic surface per D-12)
    label: string(),
    parentBigId: string().optional(), // self-FK; nullable for NULL ≠ NULL JOIN test
    createdAt: string(), // PG TIMESTAMPTZ
  })
  .primaryKey('id');

const eventTag = table('event_tags')
  .columns({
    eventId: string(),
    tagKey: string(),
    tagValue: string(),
    confidence: number(), // PG NUMERIC(5,4) (0..1; NaN-free per D-13)
    metadataJson: string(), // PG JSONB
  })
  .primaryKey('eventId', 'tagKey');

// Relationships mirroring source: every correlated subquery our test
// queries need (attachments-on-conversation, channel-on-conversation,
// participants-on-channel) must be reachable through these.
const channelRelationships = relationships(channel, ({many}) => ({
  conversations: many({
    sourceField: ['id'],
    destField: ['channelId'],
    destSchema: conversation,
  }),
  participants: many({
    sourceField: ['id'],
    destField: ['channelId'],
    destSchema: participant,
  }),
}));

const conversationRelationships = relationships(
  conversation,
  ({one, many}) => ({
    channel: one({
      sourceField: ['channelId'],
      destField: ['id'],
      destSchema: channel,
    }),
    messages: many({
      sourceField: ['id'],
      destField: ['conversationId'],
      destSchema: message,
    }),
    attachments: many({
      sourceField: ['id'],
      destField: ['conversationId'],
      destSchema: attachment,
    }),
  }),
);

const messageRelationships = relationships(message, ({one, many}) => ({
  conversation: one({
    sourceField: ['conversationId'],
    destField: ['id'],
    destSchema: conversation,
  }),
  author: one({
    sourceField: ['authorId'],
    destField: ['id'],
    destSchema: user,
  }),
  attachments: many({
    sourceField: ['id'],
    destField: ['messageId'],
    destSchema: attachment,
  }),
}));

const attachmentRelationships = relationships(attachment, ({one}) => ({
  message: one({
    sourceField: ['messageId'],
    destField: ['id'],
    destSchema: message,
  }),
  conversation: one({
    sourceField: ['conversationId'],
    destField: ['id'],
    destSchema: conversation,
  }),
}));

const participantRelationships = relationships(participant, ({one}) => ({
  channel: one({
    sourceField: ['channelId'],
    destField: ['id'],
    destSchema: channel,
  }),
  user: one({
    sourceField: ['userId'],
    destField: ['id'],
    destSchema: user,
  }),
}));

const departmentRelationships = relationships(department, ({many}) => ({
  teamMembers: many({
    sourceField: ['orgID', 'deptID'],
    destField: ['orgID', 'deptID'],
    destSchema: teamMember,
  }),
}));

const teamMemberRelationships = relationships(teamMember, ({one}) => ({
  department: one({
    sourceField: ['orgID', 'deptID'],
    destField: ['orgID', 'deptID'],
    destSchema: department,
  }),
}));

// FUZZ-02 relationships per D-09 production-shape: events↔users (actor),
// events↔conversations (relatedTicketId — repurposed FK target since the
// xyne-style schema has no tickets table), events↔event_tags (compound child).
const eventRelationships = relationships(event, ({one, many}) => ({
  actor: one({
    sourceField: ['actorUserId'],
    destField: ['id'],
    destSchema: user,
  }),
  ticket: one({
    // FK target adapted: no `tickets` table in xyne-style schema, so
    // relatedTicketId points at conversations(id). Plan acknowledges this
    // adjustment ("Adjust if FK targets in Task 1's schema.sql don't
    // exactly match"). Column name kept for plan-traceability.
    sourceField: ['relatedTicketId'],
    destField: ['id'],
    destSchema: conversation,
  }),
  tags: many({
    sourceField: ['id'],
    destField: ['eventId'],
    destSchema: eventTag,
  }),
}));

const eventTagRelationships = relationships(eventTag, ({one}) => ({
  event: one({
    sourceField: ['eventId'],
    destField: ['id'],
    destSchema: event,
  }),
}));

const bigIdRecordRelationships = relationships(bigIdRecord, ({one}) => ({
  parent: one({
    sourceField: ['parentBigId'],
    destField: ['id'],
    destSchema: bigIdRecord,
  }),
}));

export const schema = createSchema({
  tables: [
    channel,
    user,
    participant,
    conversation,
    message,
    attachment,
    department,
    teamMember,
    // FUZZ-02 additions per D-09..D-12.
    event,
    bigIdRecord,
    eventTag,
  ],
  relationships: [
    channelRelationships,
    conversationRelationships,
    messageRelationships,
    attachmentRelationships,
    participantRelationships,
    departmentRelationships,
    teamMemberRelationships,
    // FUZZ-02 relationships.
    eventRelationships,
    eventTagRelationships,
    bigIdRecordRelationships,
  ],
});

type AuthData = {
  sub: string;
};

// Permissive: anonymous can see / mutate everything. Zero's default
// when no `select` rule is declared is DENY (the query is rewritten
// as `WHERE 1=0`, collapsing to zero rows). We need explicit
// `ANYONE_CAN_DO_ANYTHING` for every table so the harness exercises
// the query pipelines, not the permission layer.
export const permissions = definePermissions<AuthData, typeof schema>(
  schema,
  () => ({
    channels: ANYONE_CAN_DO_ANYTHING,
    users: ANYONE_CAN_DO_ANYTHING,
    participants: ANYONE_CAN_DO_ANYTHING,
    conversations: ANYONE_CAN_DO_ANYTHING,
    messages: ANYONE_CAN_DO_ANYTHING,
    attachments: ANYONE_CAN_DO_ANYTHING,
    departments: ANYONE_CAN_DO_ANYTHING,
    team_members: ANYONE_CAN_DO_ANYTHING,
    // FUZZ-02 permissions — fully permissive so the harness exercises
    // pipelines, not the permission layer.
    events: ANYONE_CAN_DO_ANYTHING,
    big_id_records: ANYONE_CAN_DO_ANYTHING,
    event_tags: ANYONE_CAN_DO_ANYTHING,
  }),
);
