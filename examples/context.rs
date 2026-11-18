//! The context experiment: does a press send the agent enough to change the
//! right part of the file?
//!
//! Each case is a file with one caret prompt whose right answer is known.
//! Every run goes through Ambition's real press: a running `App` with its own
//! inbox (so `ambition more` works), a fresh document, so a fresh T3 thread,
//! and the real hook: the prompt, 8 lines each way, and nothing else. Runs
//! cannot learn from each other.
//!
//!     cargo build --release
//!     cargo run --release --example context -- <scratch folder> [reps] [workers] [model words]
//!     cargo run --release --example context -- --recheck <that folder>
//!
//! It needs T3 Code running, and it spends real model turns. Ambition 0.1
//! tried four other ways on 2026-11-16 (the paragraph above, above and below,
//! file and line only, the whole file); that code is at tag v0.1.0.

use ambition::app::App;
use ambition::carets as C;
use ambition::engine::Engine;
use ambition::inbox;
use ambition::t3::T3;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Check = fn(&str, &str) -> (bool, String);

struct Case {
    name: &'static str,
    file: &'static str,
    text: String,
    check: Check,
}

fn body(t: &str) -> String {
    t.lines().filter(|l| !l.trim_start().starts_with("^>") && !l.trim_start().starts_with("# ^>")).collect::<Vec<_>>().join("\n")
}

fn replies(t: &str) -> Vec<String> {
    t.lines().filter(|l| l.trim_start().starts_with("^>") || l.trim_start().starts_with("# ^>")).map(str::to_string).collect()
}

fn crate_cut(s: &str) -> String {
    s.chars().take(80).collect()
}

fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The text between two marker lines (exclusive), trimmed.
fn between(t: &str, a: &str, b: &str) -> Option<String> {
    let i = t.find(a)? + a.len();
    let j = t[i..].find(b)? + i;
    Some(t[i..j].trim().to_string())
}

fn cases() -> Vec<Case> {
    let mut long = String::from("# People\n\n**Ada Lovelace** — mathematician (1815)\n\n");
    for k in 1..=70 {
        long.push_str(&format!("Filler paragraph {k}: notes about the archive, the shelves, and the order of boxes.\n\n"));
    }
    long.push_str("Bob Smith, a painter, born 1990\n^ format this like the first entry at the top of the file\n");
    vec![
        Case {
            name: "shorten the paragraph above",
            file: "trip.md",
            text: "# Trip\n\nWe drove up the coast on Saturday, stopping at every overlook, every diner, and every gas station that promised clean bathrooms, until the light went orange and we finally reached the cabin.\n^ make this shorter\n\nThe cabin had no signal, which was the point.\n".into(),
            check: |before, after| {
                let b = body(after);
                let p1 = between(before, "# Trip", "The cabin had").unwrap_or_default();
                let n1 = between(&b, "# Trip", "The cabin had").unwrap_or_default();
                let ok = b.contains("The cabin had no signal, which was the point.") && b.contains("# Trip") && n1 != p1
                    && words(&n1) > 0 && words(&n1) < words(&p1);
                (ok, format!("{} -> {} words", words(&p1), words(&n1)))
            },
        },
        Case {
            name: "fill in the table below",
            file: "week.md",
            text: "# Week\n\n^ fill in this table with a plan for the week\n| Day | Plan |\n|-----|------|\n| Mon |      |\n| Tue |      |\n| Wed |      |\n".into(),
            check: |_, after| {
                let b = body(after);
                let filled = ["| Mon", "| Tue", "| Wed"].iter().all(|d| {
                    b.lines().find(|l| l.starts_with(d)).is_some_and(|l| l.split('|').nth(2).is_some_and(|c| !c.trim().is_empty()))
                });
                (filled && b.contains("# Week"), String::new())
            },
        },
        Case {
            name: "write the section under its heading",
            file: "project.md",
            text: "# Project notes\n\n## Goals\n\nShip the beta by March.\n\n## Risks\n^ write this section\n\n## Timeline\n\nBeta in March, launch in June.\n".into(),
            check: |_, after| {
                let b = body(after);
                let sec = between(&b, "## Risks", "## Timeline").unwrap_or_default();
                let ok = sec.lines().any(|l| !l.trim().is_empty() && !l.starts_with('#'))
                    && b.contains("Ship the beta by March.") && b.contains("Beta in March, launch in June.");
                (ok, format!("{} words in the section", words(&sec)))
            },
        },
        Case {
            name: "a sentence between two paragraphs",
            file: "lab.md",
            text: "The first experiment failed because the samples were contaminated.\n\n^ write one sentence that connects these two paragraphs\n\nThe second experiment used sealed containers from the start.\n".into(),
            check: |_, after| {
                let b = body(after);
                let a = "The first experiment failed because the samples were contaminated.";
                let z = "The second experiment used sealed containers from the start.";
                let mid = between(&b, a, z).unwrap_or_default();
                (b.contains(a) && b.contains(z) && words(&mid) >= 4, format!("{} words between", words(&mid)))
            },
        },
        Case {
            name: "the last list item, not the list",
            file: "groceries.md",
            text: "Groceries:\n- eggs\n- flour\n- the good olive oil, the one from the shop on the corner\n^ make this one shorter\n".into(),
            check: |_, after| {
                let b = body(after);
                let items: Vec<&str> = b.lines().filter(|l| l.starts_with("- ")).collect();
                let last = items.last().cloned().unwrap_or("");
                let ok = b.contains("- eggs\n") && b.contains("- flour\n") && items.len() == 3
                    && last != "- the good olive oil, the one from the shop on the corner" && words(last) < 12;
                (ok, last.to_string())
            },
        },
        Case {
            name: "the paragraph below, by name",
            file: "travel.md",
            text: "Intro paragraph that should stay exactly as it is.\n^ translate the paragraph below into Spanish\n\nThe weather was cold but the people were warm.\n".into(),
            // Translated in place or added under the original: both read the
            // paragraph below. The intro must stay as it is.
            check: |_, after| {
                let b = body(after);
                let ok = b.lines().next() == Some("Intro paragraph that should stay exactly as it is.")
                    && b.to_lowercase().contains("frío");
                (ok, String::new())
            },
        },
        Case {
            name: "a question gets a reply",
            file: "meeting.md",
            text: "The meeting moved from Tuesday to Thursday, then back to Wednesday because Ana could not make Thursday.\n^ what day is the meeting now?\n".into(),
            check: |before, after| {
                let ok = body(after).trim() == body(before).trim() && replies(after).iter().any(|r| r.to_lowercase().contains("wednesday"));
                (ok, replies(after).join(" / "))
            },
        },
        Case {
            name: "line 1 means the whole file",
            file: "garden.md",
            text: "^ add a title to this file\nNotes on the garden. The tomatoes need more sun. The basil is fine.\n\nThe fence on the north side is leaning.\n".into(),
            check: |_, after| {
                let b = body(after);
                let first = b.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                let ok = first.starts_with('#') && b.contains("Notes on the garden. The tomatoes need more sun. The basil is fine.")
                    && b.contains("The fence on the north side is leaning.");
                (ok, first.to_string())
            },
        },
        Case {
            name: "code: the function below",
            file: "geometry.py",
            text: "import math\n\n# ^ add type hints to the function below\ndef area(r):\n    return math.pi * r * r\n\ndef unrelated(x):\n    return x + 1\n".into(),
            check: |_, after| {
                let sig = after.lines().find(|l| l.starts_with("def area(")).unwrap_or("");
                let ok = sig.contains("->") && sig.contains(':') && sig.matches(':').count() >= 2
                    && after.contains("def unrelated(x):\n    return x + 1") && after.contains("import math");
                (ok, sig.to_string())
            },
        },
        Case {
            name: "code: a question inside a function",
            file: "total.py",
            text: "def total(items):\n    s = 0\n    for i in range(1, len(items)):\n        s += items[i]\n    # ^ why does this skip the first item?\n    return s\n".into(),
            check: |_, after| {
                let code = ["def total(items):", "    s = 0", "    for i in range(1, len(items)):", "        s += items[i]", "    return s"];
                let kept = code.iter().all(|l| after.lines().any(|x| x == *l));
                let r = replies(after).join(" ").to_lowercase();
                let ok = kept && (r.contains("range") || r.contains("index 0") || r.contains("starts at 1") || r.contains("1"));
                (ok, replies(after).join(" / "))
            },
        },
        Case {
            name: "far away: match the first entry",
            file: "people.md",
            text: long,
            check: |_, after| {
                let b = body(after);
                let bob = b.lines().find(|l| l.contains("Bob Smith")).unwrap_or("");
                let ok = bob.contains("**Bob Smith**") && bob.contains("(1990)") && bob.contains('—')
                    && b.contains("**Ada Lovelace** — mathematician (1815)") && b.matches("Filler paragraph").count() == 70;
                (ok, bob.to_string())
            },
        },
        Case {
            name: "ambiguous: this means above",
            file: "fruit.md",
            text: "Paragraph A talks about apples and how they grow in orchards.\n^ make this funnier\n\nParagraph B talks about bananas and the ships that carry them.\n".into(),
            check: |_, after| {
                let b = body(after);
                let ok = !b.contains("Paragraph A talks about apples and how they grow in orchards.")
                    && b.contains("Paragraph B talks about bananas and the ships that carry them.");
                (ok, String::new())
            },
        },
        Case {
            name: "a typo below the caret",
            file: "report.md",
            text: "The report is ready to send.\n^ fix the typo\n\nTeh numbers in section two are final.\n".into(),
            check: |_, after| {
                let b = body(after);
                let ok = b.contains("The report is ready to send.") && b.contains("The numbers in section two are final.") && !b.contains("Teh ");
                (ok, String::new())
            },
        },
        Case {
            name: "these: the whole list",
            file: "pack.md",
            text: "Things to pack:\n- tent\n- stove\n- sleeping bag\n^ sort these alphabetically\n".into(),
            check: |_, after| {
                let b = body(after);
                let items: Vec<&str> = b.lines().filter(|l| l.starts_with("- ")).collect();
                (items == ["- sleeping bag", "- stove", "- tent"], items.join(", "))
            },
        },
        Case {
            name: "summarize the long section below",
            file: "minutes.md",
            text: {
                let mut t = String::from("# Minutes\n\n^ summarize the section below in one sentence, right here\n\n## Budget discussion\n\n");
                for k in 1..=30 {
                    t.push_str(&format!("Point {k}: the team weighed cost line {k} against the travel budget and agreed to defer it.\n"));
                }
                t
            },
            // In the text or as a ^> reply at the caret: both are "right here".
            check: |before, after| {
                let b = after;
                let head = between(b, "# Minutes", "## Budget discussion").unwrap_or_default();
                let sec = before.split("## Budget discussion").nth(1).unwrap_or("").trim().to_string();
                let ok = words(&head) >= 6 && b.contains(&sec);
                (ok, crate_cut(&head))
            },
        },
        Case {
            name: "continue at the caret",
            file: "story.md",
            text: "Once upon a time, a lighthouse keeper found a letter in a bottle.\n^ continue the story\n".into(),
            check: |_, after| {
                let b = body(after);
                let s = "Once upon a time, a lighthouse keeper found a letter in a bottle.";
                let rest = b.split(s).nth(1).unwrap_or("");
                (b.contains(s) && words(rest) >= 15, format!("{} words after", words(rest)))
            },
        },
    ]
}

/// The case's file as the agent found it: the prompt out (it was a status
/// line), the rest as written.
fn before(case: &Case) -> String {
    let p = C::prompts(case.file, &case.text).remove(0);
    C::remove(&case.text, &[(p.start, p.end)]).0
}

/// Check the saved runs in `<dir>/results.json` again, with no new turns.
fn recheck(dir: &Path) {
    let cases = cases();
    let mut rows: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("results.json")).unwrap()).unwrap();
    for r in rows.iter_mut() {
        let case = cases.iter().find(|c| r["case"] == json!(c.name)).expect("a known case");
        let (ok, note) = (case.check)(&before(case), r["after"].as_str().unwrap_or(""));
        if r["ok"] != json!(ok) {
            println!("{} now {} ({note})", case.name, if ok { "passes" } else { "fails" });
        }
        r["ok"] = json!(ok);
        r["note"] = json!(note);
    }
    std::fs::write(dir.join("results.json"), serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    summary(&rows, dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().as_str());
}

fn summary(rows: &[Value], name: &str) {
    let pass = rows.iter().filter(|r| r["ok"] == true).count();
    let mut secs: Vec<f64> = rows.iter().map(|r| r["secs"].as_f64().unwrap_or(0.0)).collect();
    secs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = secs.get(secs.len() / 2).cloned().unwrap_or(0.0);
    let more = rows.iter().filter(|r| r["more"].as_u64().unwrap_or(0) > 0).count();
    for r in rows.iter().filter(|r| r["ok"] != true) {
        println!("FAIL {} ({})", r["case"].as_str().unwrap_or(""), r["note"].as_str().unwrap_or(""));
    }
    println!("\n{name}: passed {pass} of {}, median {med:.1} s a turn, `ambition more` in {more} runs", rows.len());
}

fn turns(state: &Path) -> Vec<Value> {
    ambition::app::load(&state.join("state.json"))["turns"].as_array().cloned().unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--recheck") {
        return recheck(Path::new(args.get(1).expect("the folder of a run")));
    }
    let root = PathBuf::from(args.first().expect("a scratch folder"));
    let reps: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let workers: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let want = args.get(3).cloned().unwrap_or_else(|| "sonnet 5.5".into());
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    // This experiment's own Ambition: its own state, inbox and grants.
    std::env::set_var("AMBITION_HOME", &state);
    let bin = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("ambition");
    assert!(bin.exists(), "build the binary first: cargo build --release ({})", bin.display());

    let label = format!("Ambition experiment · {}", &ambition::util::sha(&state.to_string_lossy())[..6]);
    let t3 = Arc::new(T3::new(&label));
    let engine: Arc<dyn Engine> = t3.clone();
    let model = engine.models(Some(&want)).expect("models").into_iter().next().expect("the model");
    println!("model {} ({})", model.label, model.id);
    ambition::app::save(&state.join("models.json"), &json!({"last": {"instance": model.instance, "model": model.id, "effort": null}}));

    let mut app = App::new(engine.clone());
    app.cli = format!("AMBITION_HOME={} {}", state.display(), bin.display());
    let app = Arc::new(app);
    app.start().unwrap();
    // `ambition more` reaches this process, and each call is counted.
    let more = Arc::new(Mutex::new(std::collections::BTreeMap::<String, usize>::new()));
    let (a2, m2) = (app.clone(), more.clone());
    let ib = inbox::Inbox::open().unwrap();
    std::thread::spawn(move || ib.serve(Arc::new(move |req: &Value| {
        if req["op"] == "more" {
            *m2.lock().unwrap().entry(req["cwd"].as_str().unwrap_or("").to_string()).or_default() += 1;
        }
        a2.handle(req)
    })));

    // One folder per worker, a git repo so each run's subfolder belongs to it.
    let folders: Vec<PathBuf> = (0..workers).map(|w| {
        let f = root.join(format!("worker-{w}"));
        std::fs::create_dir_all(f.join(".git")).unwrap();
        ambition::config::allow(&f).unwrap();
        std::fs::canonicalize(&f).unwrap()
    }).collect();

    let cases = Arc::new(cases());
    let mut jobs = Vec::new();
    for rep in 0..reps {
        for ci in 0..cases.len() {
            jobs.push((rep, ci));
        }
    }
    let jobs = Arc::new(Mutex::new(jobs));
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for folder in folders {
        let (jobs, results, cases, app, engine, state, more) =
            (jobs.clone(), results.clone(), cases.clone(), app.clone(), engine.clone(), state.clone(), more.clone());
        handles.push(std::thread::spawn(move || loop {
            let Some((rep, ci)) = jobs.lock().unwrap().pop() else { break };
            let case = &cases[ci];
            let dir = folder.join(format!("{ci:02}-{rep}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join(case.file);
            std::fs::write(&file, &case.text).unwrap();
            let p = C::prompts(case.file, &case.text).remove(0);
            let calls = |m: &Mutex<std::collections::BTreeMap<String, usize>>| m.lock().unwrap().iter()
                .filter(|(k, _)| Path::new(k.as_str()).starts_with(&folder)).map(|(_, v)| *v).sum::<usize>();
            let before_more = calls(&more);
            let began = Instant::now();
            let sent = app.press(&file.to_string_lossy(), p.start + 1);
            let id = sent.as_ref().ok().and_then(|s| s.split(' ').next()).unwrap_or("").to_string();
            let end = Instant::now() + Duration::from_secs(300);
            while Instant::now() < end && turns(&state).iter().any(|t| t["id"] == json!(id)) {
                std::thread::sleep(Duration::from_millis(200));
            }
            let secs = began.elapsed().as_secs_f64();
            let looked = calls(&more) - before_more;
            let after = std::fs::read_to_string(&file).unwrap_or_default();
            let (ok, note) = (case.check)(&before(case), &after);
            let entry = ambition::app::load(&state.join("threads"))[file.to_string_lossy().as_ref()].clone();
            if let Some(tid) = entry["thread"].as_str() {
                let _ = engine.settle(tid);
            }
            println!("{} {:<40} {:>5.1}s more:{looked} {note}", if ok { "pass" } else { "FAIL" }, case.name, secs);
            results.lock().unwrap().push(json!({"case": case.name, "rep": rep, "ok": ok, "note": note, "secs": secs,
                "more": looked, "sent": sent.unwrap_or_else(|e| e), "thread": entry["link"], "after": after}));
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let results = results.lock().unwrap().clone();
    std::fs::write(root.join("results.json"), serde_json::to_string_pretty(&results).unwrap()).unwrap();
    summary(&results, &model.label);
}
