---- MODULE JobLifecycle ----
\* One idlemax job on one device, as docs/manual/04-jobs.md and scheduler/update.rs describe it.
\* A lease counts as an attempt. A lease lost while attempts remain goes back to the queue, else the job is parked.
\* The machine's guard freezes a running job when the machine gets busy.
EXTENDS Naturals

CONSTANT MaxAttempts

VARIABLES status, attempts, leaseHeld, calm

vars == <<status, attempts, leaseHeld, calm>>

Statuses == {"queued", "leased", "running", "paused", "succeeded", "failed", "killed", "cancelled", "parked"}
Active == {"leased", "running", "paused"}
Final == {"succeeded", "failed", "killed", "cancelled", "parked"}

TypeOK == /\ status \in Statuses
          /\ attempts \in 0..MaxAttempts
          /\ leaseHeld \in BOOLEAN
          /\ calm \in BOOLEAN

Init == /\ status = "queued"
        /\ attempts = 0
        /\ leaseHeld = FALSE
        /\ calm = TRUE

\* A device takes the job while the machine is idle. The lease counts as an attempt.
Claim == /\ status = "queued"
         /\ calm
         /\ attempts < MaxAttempts
         /\ status' = "leased"
         /\ attempts' = attempts + 1
         /\ leaseHeld' = TRUE
         /\ UNCHANGED calm

\* The device hands the lease back before the process started. That does not count as an attempt.
ReturnUnstarted == /\ status = "leased"
                   /\ status' = "queued"
                   /\ attempts' = attempts - 1
                   /\ leaseHeld' = FALSE
                   /\ UNCHANGED calm

Start == /\ status = "leased"
         /\ calm
         /\ status' = "running"
         /\ UNCHANGED <<attempts, leaseHeld, calm>>

\* The machine gets busy. A running job freezes at once.
Busy == /\ calm
        /\ calm' = FALSE
        /\ status' = IF status = "running" THEN "paused" ELSE status
        /\ UNCHANGED <<attempts, leaseHeld>>

Idle == /\ ~calm
        /\ calm' = TRUE
        /\ UNCHANGED <<status, attempts, leaseHeld>>

\* A frozen job resumes by itself once the machine is idle.
Resume == /\ status = "paused"
          /\ calm
          /\ status' = "running"
          /\ UNCHANGED <<attempts, leaseHeld, calm>>

Succeed == /\ status = "running"
           /\ status' = "succeeded"
           /\ leaseHeld' = FALSE
           /\ UNCHANGED <<attempts, calm>>

Fail == /\ status = "running"
        /\ status' = "failed"
        /\ leaseHeld' = FALSE
        /\ UNCHANGED <<attempts, calm>>

\* Its own memory or runtime cap kills it. This is final.
Kill == /\ status = "running"
        /\ status' = "killed"
        /\ leaseHeld' = FALSE
        /\ UNCHANGED <<attempts, calm>>

\* The lease is lost, for example the device went offline. Retry while attempts remain, else park.
LoseLease == /\ status \in Active
             /\ status' = IF attempts < MaxAttempts THEN "queued" ELSE "parked"
             /\ leaseHeld' = FALSE
             /\ UNCHANGED <<attempts, calm>>

\* You cancel the job. It is final and it kills the process if one runs.
Cancel == /\ status \in {"queued", "leased", "running", "paused"}
          /\ status' = "cancelled"
          /\ leaseHeld' = FALSE
          /\ UNCHANGED <<attempts, calm>>

\* Final states have no step out. This action lets them stay put, so TLC does not report a deadlock.
Done == /\ status \in Final
        /\ UNCHANGED vars

Next == Claim \/ ReturnUnstarted \/ Start \/ Busy \/ Idle \/ Resume
        \/ Succeed \/ Fail \/ Kill \/ LoseLease \/ Cancel \/ Done

Spec == Init /\ [][Next]_vars

\* A device is held exactly while the job is leased, running or paused.
DeviceHeldExactlyWhileActive == leaseHeld <=> (status \in Active)

\* A job never runs while the machine is busy.
RunsOnlyWhenCalm == status = "running" => calm

\* A job is parked only after it used its last attempt.
ParkedOnlyAfterLastAttempt == status = "parked" => attempts = MaxAttempts
====
