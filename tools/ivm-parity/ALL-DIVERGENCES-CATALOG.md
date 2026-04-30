# Rust IVM — All Known Divergences (Master Catalog)

Total divergent observations across all sweeps: **57**
Unique AST shapes (after canonical-key dedup): **56**

Sources:

- hydrate-fuzz-multi-seed: 45
- hydrate-fuzz-big-sweep: 12

## Bucket distribution (deduped)

| Bucket                  | Count | % of unique |
| ----------------------- | ----- | ----------- |
| D-simple-OR-with-EXISTS | 22    | 39%         |
| A-nested-OR-with-EXISTS | 20    | 36%         |
| B-OR-of-AND-of-EXISTS   | 13    | 23%         |
| G-NOT-IN/LIKE-no-EXISTS | 1     | 2%          |

## Per-bucket samples (first 5 unique shapes)

### D-simple-OR-with-EXISTS (22 unique shapes)

#### 495b39912778 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), exists(1), limit, orderBy, related[], IS_NULL, NOT_IN/LIKE
- canonicalKey: `db16d5d7d3685aaa`

```json
{
  "table": "event_tags",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "IS",
        "left": {
          "type": "column",
          "name": "processedAt"
        },
        "right": {
          "type": "literal",
          "value": null
        }
      },
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "userId"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["eventId"]
          },
          "subquery": {
            "table": "event_tags",
            "alias": "arb_events_tags"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 2,
  "orderBy": [["tagKey", "desc"]],
  "related": [
    {
      "correlation": {
        "parentField": ["id"],
        "childField": ["messageId"]
      },
      "subquery": {
        "table": "attachments",
        "alias": "arb_messages_attachments_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["conversationId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "conversations",
              "alias": "arb_attachments_conversation_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### 4ef1c038a3e1 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `db16d5d7d3685aaa`

```json
{
  "table": "event_tags",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "userId"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["eventId"]
          },
          "subquery": {
            "table": "event_tags",
            "alias": "arb_events_tags"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 2,
  "orderBy": [["tagKey", "desc"]],
  "start": {
    "row": {
      "tagKey": "u1"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["id"],
        "childField": ["messageId"]
      },
      "subquery": {
        "table": "attachments",
        "alias": "arb_messages_attachments_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["conversationId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "conversations",
              "alias": "arb_attachments_conversation_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### be656334abe5 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `db16d5d7d3685aaa`

```json
{
  "table": "event_tags",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "userId"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "participants",
            "alias": "arb_channels_participants"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 2,
  "orderBy": [["tagKey", "desc"]],
  "start": {
    "row": {
      "tagKey": "u1"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["id"],
        "childField": ["messageId"]
      },
      "subquery": {
        "table": "attachments",
        "alias": "arb_messages_attachments_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["conversationId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "conversations",
              "alias": "arb_attachments_conversation_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### a739bb264a30 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `db16d5d7d3685aaa`

```json
{
  "table": "event_tags",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "userId"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "arb_channels_conversations"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 2,
  "orderBy": [["tagKey", "desc"]],
  "start": {
    "row": {
      "tagKey": "u1"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["id"],
        "childField": ["messageId"]
      },
      "subquery": {
        "table": "attachments",
        "alias": "arb_messages_attachments_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["conversationId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "conversations",
              "alias": "arb_attachments_conversation_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### d944315ba774 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), exists(1), start, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `c63521356a5b551f`

```json
{
  "table": "event_tags",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "userId"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "arb_channels_conversations"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [["tagKey", "desc"]],
  "start": {
    "row": {
      "tagKey": "u1"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["id"],
        "childField": ["messageId"]
      },
      "subquery": {
        "table": "attachments",
        "alias": "arb_messages_attachments_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["conversationId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "conversations",
              "alias": "arb_attachments_conversation_d1"
            }
          }
        ]
      }
    }
  ]
}
```

### A-nested-OR-with-EXISTS (20 unique shapes)

#### 141bc6f4af27 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), exists(1), limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `b99ed1fe119e6b75`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
          {
            "type": "simple",
            "op": "NOT IN",
            "left": {
              "type": "column",
              "name": "channelId"
            },
            "right": {
              "type": "literal",
              "value": []
            }
          },
          {
            "type": "simple",
            "op": "<",
            "left": {
              "type": "column",
              "name": "id"
            },
            "right": {
              "type": "literal",
              "value": "ch-pub-1"
            }
          }
        ]
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["eventId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "events",
            "alias": "arb_event_tags_event"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 10,
  "orderBy": [
    ["processedAt", "desc"],
    ["id", "desc"]
  ],
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "departments",
        "alias": "arb_team_members_department_d0"
      }
    }
  ]
}
```

#### c5038649ddf0 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `5b4bba23eccb3a1f`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
          {
            "type": "simple",
            "op": "NOT IN",
            "left": {
              "type": "column",
              "name": "channelId"
            },
            "right": {
              "type": "literal",
              "value": []
            }
          },
          {
            "type": "simple",
            "op": "ILIKE",
            "left": {
              "type": "column",
              "name": "name"
            },
            "right": {
              "type": "literal",
              "value": "%standup%"
            }
          }
        ]
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["eventId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "events",
            "alias": "arb_event_tags_event"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 10,
  "orderBy": [
    ["processedAt", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "processedAt": 4,
      "id": "u1"
    },
    "exclusive": false
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "departments",
        "alias": "arb_team_members_department_d0"
      }
    }
  ]
}
```

#### d469fdc20b04 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `5b4bba23eccb3a1f`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
          {
            "type": "simple",
            "op": "NOT IN",
            "left": {
              "type": "column",
              "name": "channelId"
            },
            "right": {
              "type": "literal",
              "value": []
            }
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "name"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["eventId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "events",
            "alias": "arb_event_tags_event"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 10,
  "orderBy": [
    ["processedAt", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "processedAt": 4,
      "id": "u1"
    },
    "exclusive": false
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "departments",
        "alias": "arb_team_members_department_d0"
      }
    }
  ]
}
```

#### c0e7d8685799 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), exists(1), start, limit, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `5b4bba23eccb3a1f`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
          {
            "type": "simple",
            "op": "NOT IN",
            "left": {
              "type": "column",
              "name": "channelId"
            },
            "right": {
              "type": "literal",
              "value": []
            }
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "name"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "arb_channels_conversations"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 10,
  "orderBy": [
    ["processedAt", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "processedAt": 4,
      "id": "u1"
    },
    "exclusive": false
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "departments",
        "alias": "arb_team_members_department_d0"
      }
    }
  ]
}
```

#### 757287572dbf _(source: hydrate-fuzz-multi-seed)_

- features: or(2), exists(1), start, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `5b4bba23eccb3a1f`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
          {
            "type": "simple",
            "op": "NOT IN",
            "left": {
              "type": "column",
              "name": "channelId"
            },
            "right": {
              "type": "literal",
              "value": []
            }
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "name"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "arb_channels_conversations"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["processedAt", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "processedAt": 4,
      "id": "u1"
    },
    "exclusive": false
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "departments",
        "alias": "arb_team_members_department_d0"
      }
    }
  ]
}
```

### B-OR-of-AND-of-EXISTS (13 unique shapes)

#### eb6270164b64 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), and(1), exists(2), orderBy, related[]
- canonicalKey: `f96f2d58cf91dcdc`

```json
{
  "table": "team_members",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "correlatedSubquery",
            "related": {
              "correlation": {
                "parentField": ["orgID", "deptID"],
                "childField": ["orgID", "deptID"]
              },
              "subquery": {
                "table": "team_members",
                "alias": "arb_departments_teamMembers"
              }
            },
            "op": "EXISTS"
          },
          {
            "type": "simple",
            "op": "ILIKE",
            "left": {
              "type": "column",
              "name": "tagKey"
            },
            "right": {
              "type": "literal",
              "value": "%notes"
            }
          }
        ]
      },
      {
        "type": "simple",
        "op": ">",
        "left": {
          "type": "column",
          "name": "name"
        },
        "right": {
          "type": "literal",
          "value": "ch-pub-1"
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["parentBigId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "big_id_records",
            "alias": "arb_big_id_records_parent"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["name", "desc"],
    ["id", "desc"]
  ],
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "team_members",
        "alias": "arb_departments_teamMembers_d0"
      }
    }
  ]
}
```

#### a4b9cb2e5afb _(source: hydrate-fuzz-multi-seed)_

- features: or(1), and(1), exists(2), start, orderBy, related[]
- canonicalKey: `f96f2d58cf91dcdc`

```json
{
  "table": "team_members",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "correlatedSubquery",
            "related": {
              "correlation": {
                "parentField": ["orgID", "deptID"],
                "childField": ["orgID", "deptID"]
              },
              "subquery": {
                "table": "team_members",
                "alias": "arb_departments_teamMembers"
              }
            },
            "op": "EXISTS"
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "id"
            },
            "right": {
              "type": "literal",
              "value": "ch-pub-1"
            }
          }
        ]
      },
      {
        "type": "simple",
        "op": ">",
        "left": {
          "type": "column",
          "name": "name"
        },
        "right": {
          "type": "literal",
          "value": "ch-pub-1"
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["parentBigId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "big_id_records",
            "alias": "arb_big_id_records_parent"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["name", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "name": "u1",
      "id": "u2"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "team_members",
        "alias": "arb_departments_teamMembers_d0"
      }
    }
  ]
}
```

#### 343d92f9c7bd _(source: hydrate-fuzz-multi-seed)_

- features: or(1), and(1), exists(2), start, orderBy, related[]
- canonicalKey: `f96f2d58cf91dcdc`

```json
{
  "table": "team_members",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "correlatedSubquery",
            "related": {
              "correlation": {
                "parentField": ["orgID", "deptID"],
                "childField": ["orgID", "deptID"]
              },
              "subquery": {
                "table": "team_members",
                "alias": "arb_departments_teamMembers"
              }
            },
            "op": "EXISTS"
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "id"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "simple",
        "op": ">",
        "left": {
          "type": "column",
          "name": "name"
        },
        "right": {
          "type": "literal",
          "value": "ch-pub-1"
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["parentBigId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "big_id_records",
            "alias": "arb_big_id_records_parent"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["name", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "name": "u1",
      "id": "u2"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "team_members",
        "alias": "arb_departments_teamMembers_d0"
      }
    }
  ]
}
```

#### bf68b9789a06 _(source: hydrate-fuzz-multi-seed)_

- features: or(1), and(1), exists(2), start, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `f96f2d58cf91dcdc`

```json
{
  "table": "team_members",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "correlatedSubquery",
            "related": {
              "correlation": {
                "parentField": ["orgID", "deptID"],
                "childField": ["orgID", "deptID"]
              },
              "subquery": {
                "table": "team_members",
                "alias": "arb_departments_teamMembers"
              }
            },
            "op": "EXISTS"
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "id"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "visibility"
        },
        "right": {
          "type": "literal",
          "value": ["a", "c", "a"]
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["parentBigId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "big_id_records",
            "alias": "arb_big_id_records_parent"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["name", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "name": "u1",
      "id": "u2"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "team_members",
        "alias": "arb_departments_teamMembers_d0"
      }
    }
  ]
}
```

#### eb256f1d6d8a _(source: hydrate-fuzz-multi-seed)_

- features: or(1), and(1), exists(2), start, orderBy, related[], NOT_IN/LIKE
- canonicalKey: `f96f2d58cf91dcdc`

```json
{
  "table": "team_members",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "correlatedSubquery",
            "related": {
              "correlation": {
                "parentField": ["orgID", "deptID"],
                "childField": ["orgID", "deptID"]
              },
              "subquery": {
                "table": "team_members",
                "alias": "arb_departments_teamMembers"
              }
            },
            "op": "EXISTS"
          },
          {
            "type": "simple",
            "op": "=",
            "left": {
              "type": "column",
              "name": "id"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            }
          }
        ]
      },
      {
        "type": "simple",
        "op": "NOT LIKE",
        "left": {
          "type": "column",
          "name": "id"
        },
        "right": {
          "type": "literal",
          "value": "%"
        }
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["parentBigId"],
            "childField": ["id"]
          },
          "subquery": {
            "table": "big_id_records",
            "alias": "arb_big_id_records_parent"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "orderBy": [
    ["name", "desc"],
    ["id", "desc"]
  ],
  "start": {
    "row": {
      "name": "u1",
      "id": "u2"
    },
    "exclusive": true
  },
  "related": [
    {
      "correlation": {
        "parentField": ["orgID", "deptID"],
        "childField": ["orgID", "deptID"]
      },
      "subquery": {
        "table": "team_members",
        "alias": "arb_departments_teamMembers_d0"
      }
    }
  ]
}
```

### G-NOT-IN/LIKE-no-EXISTS (1 unique shapes)

#### ce63f8de27a2 _(source: hydrate-fuzz-big-sweep)_

- features: limit, orderBy, NOT_IN/LIKE
- canonicalKey: `e60c20ec06bfa221`

```json
{
  "table": "team_members",
  "where": {
    "type": "simple",
    "op": "NOT LIKE",
    "left": {
      "type": "column",
      "name": "name"
    },
    "right": {
      "type": "literal",
      "value": "a%"
    }
  },
  "limit": 2,
  "orderBy": [["orgID", "desc"]]
}
```
