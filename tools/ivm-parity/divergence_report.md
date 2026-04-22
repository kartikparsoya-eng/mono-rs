# IVM Parity Full Sweep Results

- **Total**: 1084
- **OK**: 1047
- **Diverge**: 27
- **Error**: 10
- **Elapsed**: 221.2s

## Divergence Categories

### correlatedSubquery_EXISTS (18 ASTs)

IDs: fuzz_00079, fuzz_00080, fuzz_00083, fuzz_00084, fuzz_00130, fuzz_00131, fuzz_00132, fuzz_00133, fuzz_00134, fuzz_00135, fuzz_00138, fuzz_00139, fuzz_00140, fuzz_00150, fuzz_00151, fuzz_00154, seed_17_messages_or_cmp_and_exists_text_filter, seed_18_channels_and_of_exists_same_rel_diff_filter

#### fuzz_00079 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["channelId"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "zsubq_conversations_0"
      }
    },
    "op": "EXISTS"
  }
}
```

#### fuzz_00080 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["channelId"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "zsubq_conversations_1"
      }
    },
    "op": "EXISTS",
    "scalar": true
  }
}
```

#### fuzz_00083 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["channelId"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "zsubq_conversations_4"
      }
    },
    "op": "EXISTS",
    "flip": false
  }
}
```

#### fuzz_00084 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["channelId"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "zsubq_conversations_5"
      }
    },
    "op": "EXISTS",
    "flip": false,
    "scalar": true
  }
}
```

#### fuzz_00130 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

#### fuzz_00131 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_1"
          }
        },
        "op": "EXISTS",
        "scalar": true
      }
    ]
  }
}
```

#### fuzz_00132 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"co-1"}, {"id":"co-2"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_2"
          }
        },
        "op": "EXISTS",
        "flip": true
      }
    ]
  }
}
```

#### fuzz_00133 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"co-1"}, {"id":"co-2"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_3"
          }
        },
        "op": "EXISTS",
        "flip": true,
        "scalar": true
      }
    ]
  }
}
```

#### fuzz_00134 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_4"
          }
        },
        "op": "EXISTS",
        "flip": false
      }
    ]
  }
}
```

#### fuzz_00135 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
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
            "alias": "zsubq_conversations_5"
          }
        },
        "op": "EXISTS",
        "flip": false,
        "scalar": true
      }
    ]
  }
}
```

#### fuzz_00138 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "op": "=",
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
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

#### fuzz_00139 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "and",
    "conditions": [
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
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
            "alias": "zsubq_conversations_1"
          }
        },
        "op": "EXISTS",
        "scalar": true
      }
    ]
  }
}
```

#### fuzz_00140 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "correlatedSubquery",
        "related": {
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "conversations",
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
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
            "alias": "zsubq_conversations_1"
          }
        },
        "op": "EXISTS",
        "scalar": true
      }
    ]
  }
}
```

#### fuzz_00150 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
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
              "value": "ch-pub-1"
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
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

#### fuzz_00151 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
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
              "value": "ch-pub-1"
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
            "alias": "zsubq_conversations_0"
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

#### fuzz_00154 (ts=12, rs=14)

Diff:

```
  conversations: ts=8 rs=10
    only-rs: {"id":"x-co-4"}, {"id":"x-co-5"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "or",
        "conditions": [
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
              "value": "ch-pub-1"
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
            "alias": "zsubq_conversations_1"
          }
        },
        "op": "EXISTS",
        "scalar": true
      }
    ]
  }
}
```

#### seed_17_messages_or_cmp_and_exists_text_filter (ts=18, rs=13)

Diff:

```
  messages: ts=11 rs=6
    only-ts: {"id":"m-1"}, {"id":"m-5"}, {"id":"m-9"}, {"id":"x-m-4"}, {"id":"x-m-6"}
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "left": {
          "type": "column",
          "name": "body"
        },
        "right": {
          "type": "literal",
          "value": "%standup%"
        },
        "op": "ILIKE"
      },
      {
        "type": "simple",
        "left": {
          "type": "column",
          "name": "authorId"
        },
        "right": {
          "type": "literal",
          "value": "u1"
        },
        "op": "="
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "system": "client",
          "correlation": {
            "parentField": ["id"],
            "childField": ["messageId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "zsubq_attachments",
            "where": {
              "type": "simple",
              "left": {
                "type": "column",
                "name": "filename"
              },
              "right": {
                "type": "literal",
                "value": "%.pdf"
              },
              "op": "ILIKE"
            }
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

#### seed_18_channels_and_of_exists_same_rel_diff_filter (ts=9, rs=6)

Diff:

```
  participants: ts=6 rs=3
    only-ts: {"userId":"u1","channelId":"ch-pub-1"}, {"userId":"u1","channelId":"ch-pub-2"}, {"userId":"u1","channelId":"x-ch-deep"}
```

AST:

```json
{
  "table": "channels",
  "where": {
    "type": "and",
    "conditions": [
      {
        "type": "correlatedSubquery",
        "related": {
          "system": "client",
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "participants",
            "alias": "zsubq_participants",
            "where": {
              "type": "simple",
              "left": {
                "type": "column",
                "name": "userId"
              },
              "right": {
                "type": "literal",
                "value": "u1"
              },
              "op": "="
            }
          }
        },
        "op": "EXISTS"
      },
      {
        "type": "correlatedSubquery",
        "related": {
          "system": "client",
          "correlation": {
            "parentField": ["id"],
            "childField": ["channelId"]
          },
          "subquery": {
            "table": "participants",
            "alias": "zsubq_participants",
            "where": {
              "type": "simple",
              "left": {
                "type": "column",
                "name": "userId"
              },
              "right": {
                "type": "literal",
                "value": "u2"
              },
              "op": "="
            }
          }
        },
        "op": "EXISTS"
      }
    ]
  }
}
```

### value_level_diff (3 ASTs)

IDs: fuzz_00168, fuzz_00408, fuzz_00841

#### fuzz_00168 (ts=1, rs=1)

Diff:

```
  channels: 1 row(s) differ at value-level
    sample: {"id":"ch-priv-1"}
```

AST:

```json
{
  "table": "channels",
  "limit": 1
}
```

#### fuzz_00408 (ts=1, rs=1)

Diff:

```
  participants: 1 row(s) differ at value-level
    sample: {"userId":"u1","channelId":"ch-priv-1"}
```

AST:

```json
{
  "table": "participants",
  "limit": 1
}
```

#### fuzz_00841 (ts=1, rs=1)

Diff:

```
  messages: 1 row(s) differ at value-level
    sample: {"id":"m-1"}
```

AST:

```json
{
  "table": "messages",
  "limit": 1
}
```

### rs_returns_extra_rows (6 ASTs)

IDs: fuzz_00661, fuzz_00685, fuzz_00695, fuzz_00700, fuzz_00706, fuzz_00719

#### fuzz_00661 (ts=4, rs=18)

Diff:

```
  messages: ts=4 rs=18
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "!=",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "ch-pub-1"
    }
  }
}
```

#### fuzz_00685 (ts=4, rs=18)

Diff:

```
  messages: ts=4 rs=18
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "NOT LIKE",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "%standup%"
    }
  }
}
```

#### fuzz_00695 (ts=4, rs=18)

Diff:

```
  messages: ts=4 rs=18
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "NOT ILIKE",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "%standup%"
    }
  }
}
```

#### fuzz_00700 (ts=0, rs=14)

Diff:

```
  messages: ts=0 rs=14
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "<",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "ch-pub-1"
    }
  }
}
```

#### fuzz_00706 (ts=0, rs=14)

Diff:

```
  messages: ts=0 rs=14
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "<=",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "ch-pub-1"
    }
  }
}
```

#### fuzz_00719 (ts=4, rs=18)

Diff:

```
  messages: ts=4 rs=18
    only-rs: {"id":"m-1"}, {"id":"m-2"}, {"id":"m-3"}, {"id":"m-4"}, {"id":"m-5"}…
```

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "simple",
    "op": "!=",
    "left": {
      "type": "column",
      "name": "visibleTo"
    },
    "right": {
      "type": "literal",
      "value": "__none__"
    }
  }
}
```

## Errors (10)

### fuzz_00526 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_116"
      }
    },
    "op": "EXISTS",
    "scalar": true
  }
}
```

### fuzz_00527 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_117"
      }
    },
    "op": "EXISTS",
    "flip": true
  }
}
```

### fuzz_00528 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_118"
      }
    },
    "op": "EXISTS",
    "flip": true,
    "scalar": true
  }
}
```

### fuzz_00529 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_119"
      }
    },
    "op": "EXISTS",
    "flip": false
  }
}
```

### fuzz_00530 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_120"
      }
    },
    "op": "EXISTS",
    "flip": false,
    "scalar": true
  }
}
```

### fuzz_00531 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_121"
      }
    },
    "op": "NOT EXISTS"
  }
}
```

### fuzz_00532 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_122"
      }
    },
    "op": "NOT EXISTS",
    "scalar": true
  }
}
```

### fuzz_00533 [both]

hydration timeout after 15000ms | hydration timeout after 15000ms

AST:

```json
{
  "table": "conversations",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "correlation": {
        "parentField": ["id"],
        "childField": ["conversationId"]
      },
      "subquery": {
        "table": "messages",
        "alias": "zsubq_messages_123"
      }
    },
    "op": "NOT EXISTS",
    "flip": false
  }
}
```

### seed_22_messages_or_cmp_and_cmp_and_exists [rs]

server error: {"kind":"Internal","message":"AST translation failed for query 'seed_22_messages_or_cmp_and_cmp_and_exists': Expected CorrelatedSubquery, got And { conditions: [Simple { op: \"=\", left: Column { name: \"authorId\" }, right: Literal { value: String(\"u2\") } }, CorrelatedSubquery { related: CorrelatedSubquery { correlation: Correlation { parent_field: [\"id\"], child_field: [\"messageId\"] }, subquery: Ast { table: \"attachments\", alias: Some(\"zsubq_attachments\"), where_cond: Some(And { conditions: [Simple { op: \"ILIKE\", left: Column { name: \"filename\" }, right: Literal { value: String(\"%.png\") } }] }), related: None, limit: None, order_by: None, start: None }, hidden: None, system: Some(\"client\") }, op: \"EXISTS\", flip: None, scalar: None }] }","origin":"zeroCache"}

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "simple",
        "left": {
          "type": "column",
          "name": "visibleTo"
        },
        "right": {
          "type": "literal",
          "value": null
        },
        "op": "IS"
      },
      {
        "type": "and",
        "conditions": [
          {
            "type": "simple",
            "left": {
              "type": "column",
              "name": "authorId"
            },
            "right": {
              "type": "literal",
              "value": "u1"
            },
            "op": "="
          },
          {
            "type": "simple",
            "left": {
              "type": "column",
              "name": "conversationId"
            },
            "right": {
              "type": "literal",
              "value": "co-1"
            },
            "op": "="
          }
        ]
      },
      {
        "type": "and",
        "conditions": [
          {
            "type": "simple",
            "left": {
              "type": "column",
              "name": "authorId"
            },
            "right": {
              "type": "literal",
              "value": "u2"
            },
            "op": "="
          },
          {
            "type": "correlatedSubquery",
            "related": {
              "system": "client",
              "correlation": {
                "parentField": ["id"],
                "childField": ["messageId"]
              },
              "subquery": {
                "table": "attachments",
                "alias": "zsubq_attachments",
                "where": {
                  "type": "simple",
                  "left": {
                    "type": "column",
                    "name": "filename"
                  },
                  "right": {
                    "type": "literal",
                    "value": "%.png"
                  },
                  "op": "ILIKE"
                }
              }
            },
            "op": "EXISTS"
          }
        ]
      }
    ]
  }
}
```

### seed_23_messages_whereExists_conv_channel_or_and_exists [rs]

server error: {"kind":"Internal","message":"AST translation failed for query 'seed_23_messages_whereExists_conv_channel_or_and_exists': Expected CorrelatedSubquery, got And { conditions: [Simple { op: \"=\", left: Column { name: \"visibility\" }, right: Literal { value: String(\"private\") } }, CorrelatedSubquery { related: CorrelatedSubquery { correlation: Correlation { parent_field: [\"id\"], child_field: [\"channelId\"] }, subquery: Ast { table: \"participants\", alias: Some(\"zsubq_participants\"), where_cond: Some(And { conditions: [Simple { op: \"=\", left: Column { name: \"userId\" }, right: Literal { value: String(\"u1\") } }] }), related: None, limit: None, order_by: None, start: None }, hidden: None, system: Some(\"client\") }, op: \"EXISTS\", flip: None, scalar: None }] }","origin":"zeroCache"}

AST:

```json
{
  "table": "messages",
  "where": {
    "type": "correlatedSubquery",
    "related": {
      "system": "client",
      "correlation": {
        "parentField": ["conversationId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "zsubq_conversation",
        "where": {
          "type": "correlatedSubquery",
          "related": {
            "system": "client",
            "correlation": {
              "parentField": ["channelId"],
              "childField": ["id"]
            },
            "subquery": {
              "table": "channels",
              "alias": "zsubq_channel",
              "where": {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "left": {
                      "type": "column",
                      "name": "visibility"
                    },
                    "right": {
                      "type": "literal",
                      "value": "public"
                    },
                    "op": "="
                  },
                  {
                    "type": "and",
                    "conditions": [
                      {
                        "type": "simple",
                        "left": {
                          "type": "column",
                          "name": "visibility"
                        },
                        "right": {
                          "type": "literal",
                          "value": "private"
                        },
                        "op": "="
                      },
                      {
                        "type": "correlatedSubquery",
                        "related": {
                          "system": "client",
                          "correlation": {
                            "parentField": ["id"],
                            "childField": ["channelId"]
                          },
                          "subquery": {
                            "table": "participants",
                            "alias": "zsubq_participants",
                            "where": {
                              "type": "simple",
                              "left": {
                                "type": "column",
                                "name": "userId"
                              },
                              "right": {
                                "type": "literal",
                                "value": "u1"
                              },
                              "op": "="
                            }
                          }
                        },
                        "op": "EXISTS"
                      }
                    ]
                  }
                ]
              }
            }
          },
          "op": "EXISTS"
        }
      }
    },
    "op": "EXISTS"
  }
}
```
