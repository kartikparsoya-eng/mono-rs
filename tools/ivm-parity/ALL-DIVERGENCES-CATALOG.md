# Rust IVM — All Known Divergences (Master Catalog)

Total divergent observations across all sweeps: **6**
Unique AST shapes (after canonical-key dedup): **6**

Sources:

- hydrate-fuzz-multi-seed: 5
- hydrate-fuzz-big-sweep: 1

## Bucket distribution (deduped)

| Bucket                  | Count | % of unique |
| ----------------------- | ----- | ----------- |
| A-nested-OR-with-EXISTS | 5     | 83%         |
| D-simple-OR-with-EXISTS | 1     | 17%         |

## Per-bucket samples (first 5 unique shapes)

### A-nested-OR-with-EXISTS (5 unique shapes)

#### 23cf4c26379f _(source: hydrate-fuzz-multi-seed)_

- features: or(2), and(3), exists(2), limit, related[], NOT_IN/LIKE
- canonicalKey: `823ccb5ee6d3f0f8`

```json
{
  "table": "conversations",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "and",
            "conditions": [
              {
                "type": "simple",
                "op": "<=",
                "left": {
                  "type": "column",
                  "name": "createdAt"
                },
                "right": {
                  "type": "literal",
                  "value": 4434
                }
              },
              {
                "type": "simple",
                "op": "NOT ILIKE",
                "left": {
                  "type": "column",
                  "name": "id"
                },
                "right": {
                  "type": "literal",
                  "value": "standup%"
                }
              },
              {
                "type": "simple",
                "op": ">=",
                "left": {
                  "type": "column",
                  "name": "createdAt"
                },
                "right": {
                  "type": "literal",
                  "value": 7456
                }
              }
            ]
          },
          {
            "type": "and",
            "conditions": [
              {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "op": "NOT IN",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": [67, 65, 94]
                    }
                  },
                  {
                    "type": "simple",
                    "op": "!=",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": 2889
                    }
                  }
                ]
              },
              {
                "type": "correlatedSubquery",
                "related": {
                  "correlation": {
                    "parentField": ["channelId"],
                    "childField": ["id"]
                  },
                  "subquery": {
                    "table": "channels",
                    "alias": "arb_conversations_channel"
                  }
                },
                "op": "EXISTS"
              }
            ]
          },
          {
            "type": "simple",
            "op": ">",
            "left": {
              "type": "column",
              "name": "title"
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
            "childField": ["conversationId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "arb_conversations_attachments"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 3,
  "related": [
    {
      "correlation": {
        "parentField": ["channelId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "channels",
        "alias": "arb_participants_channel_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["id"],
              "childField": ["channelId"]
            },
            "subquery": {
              "table": "participants",
              "alias": "arb_channels_participants_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### ad9ba9641ca7 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), and(3), exists(2), limit, related[], NOT_IN/LIKE
- canonicalKey: `823ccb5ee6d3f0f8`

```json
{
  "table": "conversations",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
        "conditions": [
          {
            "type": "and",
            "conditions": [
              {
                "type": "simple",
                "op": "NOT ILIKE",
                "left": {
                  "type": "column",
                  "name": "id"
                },
                "right": {
                  "type": "literal",
                  "value": "standup%"
                }
              },
              {
                "type": "simple",
                "op": ">=",
                "left": {
                  "type": "column",
                  "name": "createdAt"
                },
                "right": {
                  "type": "literal",
                  "value": 7456
                }
              }
            ]
          },
          {
            "type": "and",
            "conditions": [
              {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "op": "NOT IN",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": [67, 65, 94]
                    }
                  },
                  {
                    "type": "simple",
                    "op": "!=",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": 2889
                    }
                  }
                ]
              },
              {
                "type": "correlatedSubquery",
                "related": {
                  "correlation": {
                    "parentField": ["channelId"],
                    "childField": ["id"]
                  },
                  "subquery": {
                    "table": "channels",
                    "alias": "arb_conversations_channel"
                  }
                },
                "op": "EXISTS"
              }
            ]
          },
          {
            "type": "simple",
            "op": ">",
            "left": {
              "type": "column",
              "name": "title"
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
            "childField": ["conversationId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "arb_conversations_attachments"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 3,
  "related": [
    {
      "correlation": {
        "parentField": ["channelId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "channels",
        "alias": "arb_participants_channel_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["id"],
              "childField": ["channelId"]
            },
            "subquery": {
              "table": "participants",
              "alias": "arb_channels_participants_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### c0e34c2f0efb _(source: hydrate-fuzz-multi-seed)_

- features: or(2), and(3), exists(2), limit, related[], NOT_IN/LIKE
- canonicalKey: `823ccb5ee6d3f0f8`

```json
{
  "table": "conversations",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
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
                  "value": "u2"
                }
              },
              {
                "type": "simple",
                "op": ">=",
                "left": {
                  "type": "column",
                  "name": "createdAt"
                },
                "right": {
                  "type": "literal",
                  "value": 7456
                }
              }
            ]
          },
          {
            "type": "and",
            "conditions": [
              {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "op": "NOT IN",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": [67, 65, 94]
                    }
                  },
                  {
                    "type": "simple",
                    "op": "!=",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": 2889
                    }
                  }
                ]
              },
              {
                "type": "correlatedSubquery",
                "related": {
                  "correlation": {
                    "parentField": ["channelId"],
                    "childField": ["id"]
                  },
                  "subquery": {
                    "table": "channels",
                    "alias": "arb_conversations_channel"
                  }
                },
                "op": "EXISTS"
              }
            ]
          },
          {
            "type": "simple",
            "op": ">",
            "left": {
              "type": "column",
              "name": "title"
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
            "childField": ["conversationId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "arb_conversations_attachments"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 3,
  "related": [
    {
      "correlation": {
        "parentField": ["channelId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "channels",
        "alias": "arb_participants_channel_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["id"],
              "childField": ["channelId"]
            },
            "subquery": {
              "table": "participants",
              "alias": "arb_channels_participants_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### b7244eaef21f _(source: hydrate-fuzz-multi-seed)_

- features: or(2), and(3), exists(2), limit, related[], NOT_IN/LIKE
- canonicalKey: `823ccb5ee6d3f0f8`

```json
{
  "table": "conversations",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
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
                  "value": "u1"
                }
              },
              {
                "type": "simple",
                "op": ">=",
                "left": {
                  "type": "column",
                  "name": "createdAt"
                },
                "right": {
                  "type": "literal",
                  "value": 7456
                }
              }
            ]
          },
          {
            "type": "and",
            "conditions": [
              {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "op": "NOT IN",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": [67, 65, 94]
                    }
                  },
                  {
                    "type": "simple",
                    "op": "!=",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": 2889
                    }
                  }
                ]
              },
              {
                "type": "correlatedSubquery",
                "related": {
                  "correlation": {
                    "parentField": ["channelId"],
                    "childField": ["id"]
                  },
                  "subquery": {
                    "table": "channels",
                    "alias": "arb_conversations_channel"
                  }
                },
                "op": "EXISTS"
              }
            ]
          },
          {
            "type": "simple",
            "op": ">",
            "left": {
              "type": "column",
              "name": "title"
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
            "childField": ["conversationId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "arb_conversations_attachments"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 3,
  "related": [
    {
      "correlation": {
        "parentField": ["channelId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "channels",
        "alias": "arb_participants_channel_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["id"],
              "childField": ["channelId"]
            },
            "subquery": {
              "table": "participants",
              "alias": "arb_channels_participants_d1"
            }
          }
        ]
      }
    }
  ]
}
```

#### 7bda308b54b1 _(source: hydrate-fuzz-multi-seed)_

- features: or(2), and(3), exists(2), limit, related[], NOT_IN/LIKE
- canonicalKey: `823ccb5ee6d3f0f8`

```json
{
  "table": "conversations",
  "where": {
    "type": "or",
    "conditions": [
      {
        "type": "and",
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
                  "value": "u1"
                }
              },
              {
                "type": "simple",
                "op": "NOT ILIKE",
                "left": {
                  "type": "column",
                  "name": "id"
                },
                "right": {
                  "type": "literal",
                  "value": "standup%"
                }
              }
            ]
          },
          {
            "type": "and",
            "conditions": [
              {
                "type": "or",
                "conditions": [
                  {
                    "type": "simple",
                    "op": "NOT IN",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": [67, 65, 94]
                    }
                  },
                  {
                    "type": "simple",
                    "op": "!=",
                    "left": {
                      "type": "column",
                      "name": "createdAt"
                    },
                    "right": {
                      "type": "literal",
                      "value": 2889
                    }
                  }
                ]
              },
              {
                "type": "correlatedSubquery",
                "related": {
                  "correlation": {
                    "parentField": ["channelId"],
                    "childField": ["id"]
                  },
                  "subquery": {
                    "table": "channels",
                    "alias": "arb_conversations_channel"
                  }
                },
                "op": "EXISTS"
              }
            ]
          },
          {
            "type": "simple",
            "op": ">",
            "left": {
              "type": "column",
              "name": "title"
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
            "childField": ["conversationId"]
          },
          "subquery": {
            "table": "attachments",
            "alias": "arb_conversations_attachments"
          }
        },
        "op": "EXISTS"
      }
    ]
  },
  "limit": 3,
  "related": [
    {
      "correlation": {
        "parentField": ["channelId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "channels",
        "alias": "arb_participants_channel_d0",
        "related": [
          {
            "correlation": {
              "parentField": ["id"],
              "childField": ["channelId"]
            },
            "subquery": {
              "table": "participants",
              "alias": "arb_channels_participants_d1"
            }
          }
        ]
      }
    }
  ]
}
```

### D-simple-OR-with-EXISTS (1 unique shapes)

#### b8b288f3f7e7 _(source: hydrate-fuzz-big-sweep)_

- features: or(1), exists(1), limit, related[], NOT_IN/LIKE
- canonicalKey: `70dc105238681877`

```json
{
  "table": "events",
  "where": {
    "type": "or",
    "conditions": [
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
      },
      {
        "type": "simple",
        "op": "NOT IN",
        "left": {
          "type": "column",
          "name": "processedAt"
        },
        "right": {
          "type": "literal",
          "value": []
        }
      }
    ]
  },
  "limit": 6,
  "related": [
    {
      "correlation": {
        "parentField": ["conversationId"],
        "childField": ["id"]
      },
      "subquery": {
        "table": "conversations",
        "alias": "arb_messages_conversation_d0"
      }
    }
  ]
}
```
