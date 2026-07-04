import Foundation

// Fixture provenance: captured 2026-07-04 with a read-only
// `GET http://127.0.0.1:3456/llmux/dashboard` against a REAL local daemon —
// `llmux 0.2.14 (preview 2026-07-04-0558-4cc97acd45dc)`, i.e. the exact
// serializer in `src/dashboard.rs` at master 4cc97ac — then trimmed (fewer
// accounts / model / client / windowed / activity rows) and scrubbed (emails →
// userN@example.com, client ids → client-N, config_path → /Users/user/…).
// Structure and field spelling are untouched wire output, including the
// `kind`-tagged `completed` entries (`request` + `note`) and a `null`-free vs
// `null`-carrying mix of optional fields.
enum DashboardFixtures {
    /// A full, current-daemon dashboard document.
    static let full = """
{
  "version": "llmux 0.2.14 (preview 2026-07-04-0558-4cc97acd45dc)",
  "pid": 15555,
  "uptime_secs": 935,
  "port": 3456,
  "current": "claude:user1@example.com",
  "current_by_group": {
    "claude": "claude:user1@example.com",
    "codex": "codex:user1@example.com"
  },
  "upstream": "https://api.anthropic.com",
  "config_path": "/Users/user/.config/llmux.json",
  "select_params": {
    "five_hour_max": 0.9,
    "seven_day_max": 0.99,
    "usage_max_age_secs": 600
  },
  "refresh_ahead_secs": 25200,
  "evaluate_tick_secs": 60,
  "accounts": [
    {
      "name": "claude:user1@example.com",
      "type": "oauth",
      "status": "active",
      "order": 2,
      "blocked": null,
      "healthy": true,
      "five_hour": {
        "utilization": 0.06,
        "resets_at": 1783159200,
        "resets_in_secs": 13088,
        "fetched_at_ms": 1783146111454,
        "source": "headers"
      },
      "seven_day": {
        "utilization": 0.61,
        "resets_at": 1783155600,
        "resets_in_secs": 9488,
        "fetched_at_ms": 1783146111454,
        "source": "headers"
      },
      "fable_weekly": {
        "utilization": 0.91,
        "resets_at": 1783155599,
        "resets_in_secs": 9488,
        "severity": "critical",
        "is_active": true,
        "constraining": false
      },
      "scoped_limits": [
        {
          "scope_label": "Fable",
          "utilization": 0.91,
          "resets_at": 1783155599,
          "resets_in_secs": 9488,
          "severity": "critical",
          "is_active": true,
          "constraining": false
        }
      ],
      "cooldown_until": null,
      "cooldown_source": null,
      "in_flight": 1,
      "token_expires_at_ms": 1783170183305,
      "last_refresh_ms": 1783141383305,
      "totals": {
        "requests": 200,
        "input_tokens": 39236,
        "output_tokens": 77940
      },
      "session": {
        "requests": 5766,
        "ok": 5687,
        "errors": 79,
        "tokens_in": 1470378,
        "tokens_out": 3174425
      }
    },
    {
      "name": "codex:user1@example.com",
      "type": "codex",
      "status": "active",
      "order": 1,
      "blocked": null,
      "healthy": true,
      "five_hour": {
        "utilization": 0.13,
        "resets_at": 1783148125,
        "resets_in_secs": 2013,
        "fetched_at_ms": 1783145180138,
        "source": "headers"
      },
      "seven_day": {
        "utilization": 0.1,
        "resets_at": 1783389705,
        "resets_in_secs": 243593,
        "fetched_at_ms": 1783145180138,
        "source": "headers"
      },
      "fable_weekly": null,
      "scoped_limits": [],
      "cooldown_until": null,
      "cooldown_source": null,
      "in_flight": 0,
      "token_expires_at_ms": 1783365005000,
      "last_refresh_ms": 1782501005584,
      "totals": {
        "requests": 0,
        "input_tokens": 0,
        "output_tokens": 0
      },
      "session": {
        "requests": 917,
        "ok": 888,
        "errors": 29,
        "tokens_in": 31251831,
        "tokens_out": 362640
      }
    },
    {
      "name": "codex:user4@example.com",
      "type": "codex",
      "status": "ok",
      "order": 12,
      "blocked": "7d 100.0% > 99%",
      "healthy": true,
      "five_hour": {
        "utilization": 0.0,
        "resets_at": 1783163180,
        "resets_in_secs": 17068,
        "fetched_at_ms": 1783145180942,
        "source": "headers"
      },
      "seven_day": {
        "utilization": 1.0,
        "resets_at": 1783388609,
        "resets_in_secs": 242497,
        "fetched_at_ms": 1783145180942,
        "source": "headers"
      },
      "fable_weekly": null,
      "scoped_limits": [],
      "cooldown_until": null,
      "cooldown_source": null,
      "in_flight": 0,
      "token_expires_at_ms": 1783977783000,
      "last_refresh_ms": 1783113783256,
      "totals": {
        "requests": 0,
        "input_tokens": 0,
        "output_tokens": 0
      },
      "session": {
        "requests": 11,
        "ok": 11,
        "errors": 0,
        "tokens_in": 124196,
        "tokens_out": 7421
      }
    }
  ],
  "scheduler": {
    "last_switch": {
      "from": "codex:user4@example.com",
      "to": "codex:user1@example.com",
      "reason": "re-evaluation",
      "at_ms": 1783145238901
    },
    "next_in_line": "claude:user2@example.com",
    "next_eval_in_secs": 25
  },
  "poller": [
    {
      "account": "user4@example.com",
      "last_ok_ms": 1783145807786,
      "consecutive_failures": 0,
      "next_at_ms": 1783146136406
    },
    {
      "account": "user5@example.com",
      "last_ok_ms": 1783145859524,
      "consecutive_failures": 0,
      "next_at_ms": 1783146165704
    }
  ],
  "totals": {
    "requests": 36633,
    "ok": 35621,
    "errors": 1012,
    "tokens_in": 79943758,
    "tokens_out": 29670001,
    "rpm_5m": 16.0,
    "in_flight": 1,
    "cost_usd": 8689.70841815
  },
  "model_usage": [
    {
      "group": "claude",
      "model": "claude-opus-4-8",
      "requests": 30226,
      "ok": 29720,
      "errors": 506,
      "tokens_in": 11366225,
      "tokens_out": 25550294,
      "cache_read": 4889342877,
      "cache_creation": 486121851,
      "last_used_ms": 1783146111455,
      "in_flight": 1,
      "accounts": [
        {
          "name": "user9@example.com",
          "requests": 6949,
          "ok": 6862,
          "errors": 87,
          "tokens_in": 3578850,
          "tokens_out": 6733525
        },
        {
          "name": "claude:user6@example.com",
          "requests": 4108,
          "ok": 4097,
          "errors": 11,
          "tokens_in": 1601547,
          "tokens_out": 3401556
        }
      ],
      "efforts": [
        {
          "label": "none",
          "requests": 29632
        },
        {
          "label": "31k",
          "requests": 594
        }
      ],
      "endpoints": [
        {
          "label": "messages",
          "requests": 28514
        },
        {
          "label": "count_tokens",
          "requests": 1712
        }
      ]
    },
    {
      "group": "codex",
      "model": "gpt-5.5",
      "requests": 1354,
      "ok": 1324,
      "errors": 30,
      "tokens_in": 65402313,
      "tokens_out": 636050,
      "cache_read": 53827840,
      "last_used_ms": 1783136257388,
      "in_flight": 0,
      "accounts": [
        {
          "name": "codex:user1@example.com",
          "requests": 917,
          "ok": 888,
          "errors": 29,
          "tokens_in": 31251831,
          "tokens_out": 362640
        },
        {
          "name": "user1@example.com",
          "requests": 241,
          "ok": 241,
          "errors": 0,
          "tokens_in": 28201847,
          "tokens_out": 158825
        }
      ],
      "efforts": [
        {
          "label": "xhigh",
          "requests": 744
        },
        {
          "label": "none",
          "requests": 610
        }
      ],
      "endpoints": [
        {
          "label": "messages",
          "requests": 1206
        },
        {
          "label": "count_tokens",
          "requests": 148
        }
      ]
    }
  ],
  "client_usage": [
    {
      "client": "unknown",
      "requests": 11839,
      "ok": 11216,
      "errors": 623,
      "tokens_in": 62996939,
      "tokens_out": 7053653
    },
    {
      "client": "client-2",
      "requests": 1825,
      "ok": 1776,
      "errors": 49,
      "tokens_in": 361039,
      "tokens_out": 1357119
    },
    {
      "client": "client-3",
      "requests": 1113,
      "ok": 1108,
      "errors": 5,
      "tokens_in": 287288,
      "tokens_out": 846936
    }
  ],
  "windowed": [
    {
      "window": "24h",
      "window_secs": 86400,
      "cells": [
        {
          "group": "claude",
          "model": "claude-opus-4-8",
          "account": "claude:user1@example.com",
          "requests": 2259,
          "ok": 2256,
          "errors": 3,
          "tokens_in": 352514,
          "tokens_out": 705761,
          "cache_read": 197188652,
          "cache_creation": 12797900,
          "tokens": 211044827
        },
        {
          "group": "claude",
          "model": "claude-fable-5",
          "account": "claude:user3@example.com",
          "requests": 702,
          "ok": 702,
          "errors": 0,
          "tokens_in": 291878,
          "tokens_out": 567516,
          "cache_read": 134086199,
          "cache_creation": 10366078,
          "tokens": 145311671
        }
      ]
    },
    {
      "window": "72h",
      "window_secs": 259200,
      "cells": [
        {
          "group": "claude",
          "model": "claude-fable-5",
          "account": "claude:user1@example.com",
          "requests": 1488,
          "ok": 1483,
          "errors": 5,
          "tokens_in": 472278,
          "tokens_out": 1091437,
          "cache_read": 384268795,
          "cache_creation": 31076522,
          "tokens": 416909032
        },
        {
          "group": "claude",
          "model": "claude-opus-4-8",
          "account": "claude:user1@example.com",
          "requests": 3348,
          "ok": 3340,
          "errors": 8,
          "tokens_in": 618010,
          "tokens_out": 1268189,
          "cache_read": 308645584,
          "cache_creation": 19518296,
          "tokens": 330050079
        }
      ]
    }
  ],
  "activity": {
    "in_flight": [
      {
        "id": 201,
        "method": "POST",
        "path": "/v1/messages?beta=true",
        "account": "claude:user1@example.com",
        "started_at_ms": 1783146110473,
        "group": "claude",
        "model": "claude-opus-4-8"
      }
    ],
    "completed": [
      {
        "kind": "request",
        "at_ms": 1783146111455,
        "method": "POST",
        "path": "/v1/messages?beta=true",
        "account": "claude:user1@example.com",
        "status": 200,
        "duration_ms": 1463,
        "tokens": {
          "input": 106,
          "output": 7
        },
        "cost_usd": 0.025631,
        "group": "claude",
        "model": "claude-opus-4-8"
      },
      {
        "kind": "request",
        "at_ms": 1783146110489,
        "method": "POST",
        "path": "/v1/messages?beta=true",
        "account": "claude:user1@example.com",
        "status": 200,
        "duration_ms": 9752,
        "tokens": {
          "input": 2,
          "output": 617
        },
        "cost_usd": 0.143132,
        "group": "claude",
        "model": "claude-fable-5"
      },
      {
        "kind": "note",
        "at_ms": 1783145779306,
        "text": "token refreshed: claude:user3@example.com (expires 7h59m)",
        "error": false
      }
    ]
  },
  "logs": [
    {
      "level": "INFO",
      "text": "llmux daemon ready"
    }
  ],
  "codex": {
    "available": true,
    "fast": true,
    "model": "gpt-5.5"
  },
  "email_anonymous": false,
  "show_fable_weekly": true
}
"""
}
