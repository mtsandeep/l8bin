# Litebin Demo App — Final Reference Plan

## 🧠 Goal of the Demo

Build a **lightweight, interactive system** that:

* Feels like a real app (not static demo)
* Works with very low traffic (1–5 users)
* Demonstrates:

  * controlled resource usage
  * system stability under spam/load
  * backend authority vs frontend optimism
* Never crashes the VPS

---

# 🧩 Core Concept

A **team-based interactive game**:

* 🔴 Red vs 🔵 Blue
* Shared global counter
* Lucky zone (hold mechanic)
* Mini-games (balloon + math)
* Powerups (freeze, fire, boost, reverse)

---

# 🧱 System Architecture Overview

| Layer    | Responsibility                           |
| -------- | ---------------------------------------- |
| Frontend | UX, animations, optimistic feedback      |
| Backend  | game logic, validation, throttling       |
| DB       | minimal persistence, periodic state save |

---

# 🎮 GAME MECHANICS

## 1. Core Loop

* Users click → increment counter
* Teams compete to control lucky zone
* System runs continuous timed cycles:

  * mini-games (10s on / 10s off)
  * powerups (random spawn)

---

## 2. Lucky Zone

* Appears as number range (e.g. 1200–1210)
* Assigned to a team
* If counter stays inside zone for X seconds:
  → team wins bonus

---

## 3. Mini-Games (Engagement Layer)

### Cycle:

* 10s active → 10s inactive → repeat

### Types:

* 🎈 Balloon → rapid clicks
* 🧮 Math → solve simple equation

### Claim Model:

* anyone can complete
* frontend always shows reward
* backend caps total applied reward per team

---

## 4. Powerups (System Behavior Layer)

### Types:

* ❄️ Freeze → blocks opponent clicks (can break faster)
* 🔥 Fire → drains opponent score over time (clicks reduce duration)
* ⚡ Boost → multiplier for clicks
* 🔁 Reverse → opponent clicks go opposite

---

### Rules:

* only 1 active at a time
* random spawn
* first team to claim activates

---

# 🧠 FRONTEND LOGIC (UX Layer)

## Responsibilities

### 1. Instant Feedback

* clicks feel immediate
* show rewards instantly:
  → “+50 earned”

---

### 2. Visual State

Display:

* counter
* team scores
* lucky zone
* active powerup
* mini-game UI
* timers/countdowns

---

### 3. Optimistic UI

* assume success
* update UI instantly
* later corrected by backend state

---

### 4. Polling System

* fetch `/state` every:

  * 1s (normal)
  * 2–3s (under load)
* backend controls interval

---

### 5. Mini-Game Interaction

* render balloon/math UI
* send result to backend
* always show success feedback

---

### 6. Sync Correction

* backend is source of truth
* UI updates based on polled state
* smooth transitions (avoid jumps)

---

### 7. Load Awareness UI

Show subtle signals:

* “System busy — slowing updates”
* “Team cap reached”

---

---

# 🔴 BACKEND LOGIC (Core Engine)

## Responsibilities

---

## 1. Source of Truth

Backend controls:

* counter
* scores
* lucky zone
* mini-game state
* powerups
* claim limits

---

## 2. Click Handling

* accept simple requests:
  → `{ team: "red" }`
* increment in memory
* batch updates (every ~300–500ms)

---

## 3. Throttling & Protection

### Per-user:

* rate limit (e.g. 20 req/sec)

### Global:

* max active requests
* drop excess input

---

## 4. Aggregation Model

* collect clicks in memory
* periodically apply:
  → update counter
  → write to DB

---

## 5. Mini-Game Validation

* verify correctness (math)
* verify timing
* apply reward if within cap

---

## 6. Claim Limiting

Per game:

```text
maxClaimsPerTeam = 3–5
```

* always return success to user
* only apply reward if under cap

---

## 7. Powerup Engine

* spawn randomly
* track active power
* apply effect globally
* expire after duration

---

## 8. Game Loop (Tick System)

Runs every ~100–300ms:

* update timers
* process lucky zone
* process powerups
* process mini-game lifecycle

---

## 9. Adaptive Poll Control

Return:

```json
{
  state,
  pollInterval
}
```

Based on:

* request load
* system pressure

---

---

# 💾 DATABASE ROLE (Minimal Persistence)

## Philosophy:

> DB is for durability, NOT real-time logic

---

## What to store:

### 1. Game State Snapshot

* counter
* team scores
* timestamps

---

### 2. Optional Metrics

* total clicks
* resets count

---

## What NOT to store:

* per-user actions
* click history
* mini-game attempts
* powerup logs

---

## Write Strategy:

* batch writes (every few seconds)
* avoid per-click writes

---

---

# ⚡ PERFORMANCE STRATEGY

## Key Techniques:

* in-memory state (primary)
* batched DB writes
* polling (no WebSockets)
* capped rewards
* rate limiting

---

## Result:

* stable under spam
* predictable resource usage
* no crashes

---

# 🎯 DEMO STORY (What user experiences)

1. Opens app → sees loading (wake simulation)
2. Starts clicking → counter moves
3. Sees:

   * lucky zone
   * mini-game popup
4. Plays mini-game → gets reward
5. Sees system react:

   * scores update
   * powerups trigger
6. Under load:

   * throttling kicks in
   * system slows but survives

---

# 🧠 CORE PRINCIPLE

> Accept all user input → control what actually affects the system

---

# 💡 FINAL TAKEAWAY

This demo is not about scale.

It demonstrates:

* **resilience under constraints**
* **controlled resource usage**
* **graceful degradation**

---

## One-line summary:

> A shared interactive game where user actions are accepted optimistically but applied selectively to keep the system stable.

---
