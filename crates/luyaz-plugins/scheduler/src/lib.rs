//! The scheduler component: `luyaz_policy::scheduler::due` behind `wit/scheduler.wit`. Only type
//! conversion here; the policy and its tests live in u60-policy. A job arrives with what `due`
//! reads (see the wit header); the action, name and status fields are filled in empty.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/scheduler.wit", world: "scheduler" });

use luyaz_policy::scheduler as p;

fn job(j: Job) -> p::Job {
    p::Job {
        id: j.id,
        name: String::new(),
        enabled: j.enabled,
        schedule: match j.schedule {
            Schedule::Once(at) => p::Schedule::Once { at },
            Schedule::Recurring(r) => p::Schedule::Recurring {
                time: r.time,
                days: r.days,
            },
        },
        action: p::Action {
            method: String::new(),
            path: String::new(),
            body: None,
        },
        restore: j.restore_time.map(|time| p::Restore { time, body: None }),
        last_run: j.last_run,
        last_status: None,
        last_error: None,
        last_restore: j.last_restore,
        last_restore_status: None,
        last_restore_error: None,
        created_at: 0,
    }
}

struct Policy;

impl Guest for Policy {
    fn due(jobs: Vec<Job>, now_ts: i64, hh: u8, mm: u8, dow: u8) -> Vec<Fire> {
        let jobs: Vec<p::Job> = jobs.into_iter().map(job).collect();
        p::due(&jobs, now_ts, hh, mm, dow)
            .into_iter()
            .map(|(id, restore)| Fire { id, restore })
            .collect()
    }
}

export!(Policy);
