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

// ---------------------------------------------------------------------------
// FUZZ-02 (Phase 34) — production-shape rich-type tables.
// CONTEXT D-09/D-10/D-11/D-12.
//
// Zero only exposes string/number/boolean primitives at the schema level. Rich
// PG types (jsonb, timestamptz, numeric, bigint > 2^53) replicate as TEXT in
// the wire protocol. The `string()` declarations below carry the wire-format
// representation; the schema.sql side declares the real PG type.
// ---------------------------------------------------------------------------

// `events` — exercises jsonb / timestamptz / numeric / nullable / boolean.
const event = table('events')
  .columns({
    id: string(),
    occurredAt: string(), // PG TIMESTAMPTZ — replicated as ISO 8601 string
    metadataJson: string(), // PG JSONB — replicated as JSON-string
    amount: string(), // PG NUMERIC(20,6) — replicated as string (preserve precision)
    quantity: number(), // PG BIGINT — values stay within f64 safe range here
    isProcessed: boolean(),
    actorUserId: string().optional(), // FK to users; NULL allowed (NULL semantics)
    notes: string().optional(), // nullable text
    relatedTicketId: string().optional(), // self-referential dangling FK; ticket table not present (use NULL)
  })
  .primaryKey('id');

// `big_id_records` — i64 > 2^53 organic surface for B8/B9 (CONTEXT D-12).
// IDs stored as TEXT in PG; numeric semantics live in seed data.
const bigIdRecord = table('big_id_records')
  .columns({
    id: string(),
    label: string(),
    parentBigId: string().optional(), // self-FK NULL allowed (NULL ≠ NULL in JOIN keys)
    createdAt: string(),
  })
  .primaryKey('id');

// `event_tags` — compound key + jsonb for partition-Take coverage (B3 fuzz).
const eventTag = table('event_tags')
  .columns({
    eventId: string(),
    tagKey: string(),
    tagValue: string(),
    confidence: number(), // PG NUMERIC(5,4); 0..1
    metadataJson: string(), // PG JSONB
  })
  .primaryKey('eventId', 'tagKey');

const eventRelationships = relationships(event, ({one, many}) => ({
  actor: one({
    sourceField: ['actorUserId'],
    destField: ['id'],
    destSchema: user,
  }),
  tags: many({
    sourceField: ['id'],
    destField: ['eventId'],
    destSchema: eventTag,
  }),
}));

const eventTagRelationships = relationships(eventTag, ({one}) => ({
  event: one({sourceField: ['eventId'], destField: ['id'], destSchema: event}),
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
    // FUZZ-02 additions:
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
    // FUZZ-02 additions:
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
    // FUZZ-02 additions:
    events: ANYONE_CAN_DO_ANYTHING,
    big_id_records: ANYONE_CAN_DO_ANYTHING,
    event_tags: ANYONE_CAN_DO_ANYTHING,
  }),
);
