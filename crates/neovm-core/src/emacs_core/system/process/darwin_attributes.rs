//! Darwin's GNU attribute surface (sysdep.c:4273-4459). Native CPU times are
//! nanoseconds and start times are epoch microseconds, not Linux clock ticks.
use super::*;

pub(super) fn attributes(pid: i64) -> EvalResult {
    let Some(snapshot) = sys::darwin_process_snapshot(pid) else {
        return Ok(Value::NIL);
    };
    let mut attrs = Vec::new();
    let mut add = |key: &str, value| attrs.push(Value::cons(Value::symbol(key), value));
    add("euid", Value::fixnum(i64::from(snapshot.euid)));
    if let Some(user) = sys::user_name(snapshot.euid) {
        add("user", Value::string(user));
    }
    add("egid", Value::fixnum(i64::from(snapshot.egid)));
    if let Some(group) = sys::group_name(snapshot.egid) {
        add("group", Value::string(group));
    }
    add("comm", Value::string(snapshot.comm));
    add("state", Value::string(snapshot.state));
    add("ppid", Value::fixnum(snapshot.ppid));
    add("pgrp", Value::fixnum(snapshot.pgrp));
    if let Some(tty) = snapshot.tty_name {
        add("ttname", Value::string(tty));
    }
    add("tpgid", Value::fixnum(snapshot.tpgid));
    if let Some(usage) = snapshot.usage {
        let cpu_time = |time: sys::DarwinCpuTime| {
            let (seconds, nanos) = time.seconds_and_nanoseconds();
            Value::list(vec![
                Value::fixnum((seconds >> 16) as i64),
                Value::fixnum((seconds & 0xffff) as i64),
                Value::fixnum(i64::from(nanos / 1000)),
                Value::fixnum(i64::from(nanos % 1000) * 1000),
            ])
        };
        add("utime", cpu_time(usage.user));
        add("stime", cpu_time(usage.system));
        if let Some(total) = usage.user.checked_add(usage.system) {
            add("time", cpu_time(total));
        }
        add("majflt", Value::fixnum(usage.pageins as i64));
    }
    add("nice", Value::fixnum(snapshot.nice));
    let start = (
        snapshot.start.seconds,
        i64::from(snapshot.start.microseconds),
    );
    add("start", time_list_from_secs_usecs(start.0, start.1));
    let elapsed = now_epoch_secs_usecs()
        .map(|now| nonnegative_time_diff(now, start))
        .unwrap_or((0, 0));
    add("etime", time_list_from_secs_usecs(elapsed.0, elapsed.1));
    if let Some(memory) = snapshot.memory {
        add("vsize", Value::fixnum(memory.virtual_kilobytes as i64));
        add("rss", Value::fixnum(memory.resident_kilobytes as i64));
        add("thcount", Value::fixnum(i64::from(memory.threads)));
    }
    if let Some(args) = snapshot.args {
        add("args", Value::string(args));
    }
    // GNU prepends each attribute as it is gathered.
    attrs.reverse();
    Ok(Value::list(attrs))
}
