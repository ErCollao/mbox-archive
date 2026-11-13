#!/usr/bin/env python3
"""Generate a deterministic, fictional ~400-message Gmail-Takeout-style mbox
for the disposable test environment (screenshots, demos, perf smoke).

Every address uses the reserved .test TLD; no real person, domain or data.
Usage:  python3 test-env/fixtures/make-demo-mbox.py [out]   (default: demo.mbox)
"""
import random
import sys
import time
from datetime import datetime, timedelta

random.seed(20261112)

PEOPLE = [
    ("Tester Primary", "tester-primary@sample.test"),
    ("Tester Alias", "tester-alias@sample.test"),
]
FRIENDS = [
    ("Alice Ferreira", "alice@sample.test"),
    ("Bob Sandoval", "bob@northwind.test"),
    ("Carla Jensen", "carla@gardenclub.test"),
    ("Miguel Ortega", "miguel@family.test"),
    ("Priya Nair", "priya@bookclub.test"),
    ("Tom Weller", "tom@trails.test"),
    ("Nora Lindqvist", "nora@designstudio.test"),
    ("Sam Okafor", "sam@northwind.test"),
]
SERVICES = [
    ("The Weekly Standard", "news@weekly-standard.test"),
    ("Lumen Hosting", "support@lumenhost.test"),
    ("Northwind Payroll", "payroll@northwind.test"),
    ("GreenGrocer Co-op", "hello@greengrocer.test"),
    ("Trail Pass Office", "permits@trails.test"),
]

TOPICS = [
    ("Ledger migration plan", "work", 5), ("Q4 budget review", "work", 4),
    ("Standup notes thread", "work", 6), ("Vendor contract renewal", "work", 3),
    ("Design review: sidebar", "work", 4), ("Offsite logistics", "work", 5),
    ("Invoice #4021 question", "work", 3), ("Roadmap sync", "work", 4),
    ("Garden beds spring plan", "family", 3), ("Sunday lunch menu", "family", 4),
    ("Miguel's birthday", "family", 5), ("Cottage weekend", "family", 3),
    ("Book club: March pick", "social", 4), ("Board game night", "social", 3),
    ("Hiking trip votes", "social", 5), ("Potluck sign-up", "social", 2),
    ("Reef tank nitpicks", "social", 3), ("Photography walk", "social", 2),
    ("Weekly digest", "news", 40), ("Hosting renewal notice", "logistics", 2),
    ("Payroll stub December", "logistics", 1), ("Trail permit confirmed", "logistics", 1),
    ("CSAM monthly report", "news", 20), ("Co-op delivery schedule", "news", 8),
    ("Password reset link", "logistics", 1), ("Shipping confirmation", "logistics", 1),
    ("Passing: old desk lamp", "social", 2), ("Lending you the drill", "social", 1),
    ("Roof quote follow-up", "logistics", 3), ("School fete committee", "social", 4),
    ("Piano lessons schedule", "family", 2), ("Dentist rebooking", "logistics", 1),
    ("Fiber upgrade install", "logistics", 2), ("Secondhand bike deal", "social", 3),
    ("Olive tree overstock", "family", 2), ("Retrospective action items", "work", 3),
    ("Onboarding buddy swap", "work", 2), ("API deprecation notice", "work", 3),
]

OPENERS = [
    "Hi {to},\n\n{body}\n\nCheers,\n{fromn}",
    "Hey {to},\n\n{body}\n\nThanks!\n{fromn}",
    "{body}\n\n— {fromn}",
    "Hello {to}!\n\n{body}\n\nBest,\n{fromn}",
]
REPLIES = [
    "Good point. {body}",
    "Agreed on all counts. {body}",
    "+1. {body}",
    "Not sure about that one — {body}",
    "Let me think about it. {body}",
    "Perfect, that works for me. {body}",
]
SENTENCES = {
    "work": [
        "The migration dry-run finished cleanly overnight, no schema drift.",
        "Can you take the draft review? I'm out Thursday afternoon.",
        "Numbers moved the way we predicted; variance is under two percent.",
        "I pushed the revised timeline to the shared sheet — comments open until Friday.",
        "The vendor came back with a counteroffer on the multi-year term.",
        "We should loop in finance before anything gets signed.",
        "Retro summary attached, action items bolded.",
        "Standing suggestion: fewer recurring meetings, more written updates.",
    ],
    "family": [
        "The beds are turned and the soil finally drained after last week's rain.",
        "Same time Sunday? I'll bring the bread this round.",
        "He says he only wants the blue ones — small tyrant.",
        "Cottage is warm, the stove pulls hard, and the wifi is gloriously bad.",
        "We're leaning toward Saturday but confirm after the school fete.",
        "Left the keys under the terracotta pot like always.",
        "Ripened on the counter, sweet as hell. You need to try one.",
    ],
    "social": [
        "March pick is doing surprisingly well — chapter nine wrecked me.",
        "Board game night stands unless the storm warning gets worse.",
        "I vote for the river loop; the ridge trail kills beginners.",
        "Bringing the potato salad and two bottles of something cold.",
        "Golden hour at the reservoir was worth the early alarm.",
        "The drill is charged; take your time, we won't need it till next month.",
        "Six of the seven bikes are still up for grabs this evening.",
    ],
    "news": [
        "This week: a long read on slow cities, three recipes under thirty "
        "minutes, and our annual gear roundup.",
        "Top story — the harbor library project cleared its final review.",
        "Also in the archive: ten lessons from a decade of community gardening.",
        "Your feed has 12 new posts from people and pages you follow.",
        "The co-op harvest calendar is out; kale season officially begins.",
        "Members' notes: our editor answers the question everyone asked.",
    ],
    "logistics": [
        "Your renewal is confirmed for another twelve months at the current rate.",
        "Permit #TP-2214 attached. Please carry a printed or offline copy.",
        "Delivery window is Thursday 08:00–12:00; leave space at the gate.",
        "No action needed unless you want a different plan before the term ends.",
        "The technician arrives next Tuesday; the confirmation code is below.",
        "Tracking shows out for delivery tomorrow, signature not required.",
    ],
}
QUOTES = [
    "The migration dry-run finished cleanly overnight",
    "I vote for the river loop",
    "variance is under two percent",
    "chapter nine wrecked me",
    "renewal is confirmed for another twelve months",
]

def rand_date():
    start = datetime(2024, 1, 2)
    end = datetime(2026, 11, 1)
    delta = (end - start).days
    d = start + timedelta(days=random.randrange(delta),
                          hours=random.randrange(7, 22),
                          minutes=random.randrange(60))
    return d

def fmt_date(d):
    return d.strftime("%a, %d %b %Y %H:%M:%S +0000")

def from_line(d):
    return time.asctime(d.timetuple())

def body_for(kind, name):
    s = random.sample(SENTENCES[kind], k=random.choice([2, 3]))
    return name + " here — " + " ".join(s) if random.random() < 0.3 else "\n\n".join(s)

msgs = []
mid = 0

def add(thread_id, frm, to, cc, subject, date, kind, labels, html=False, attach=None, quote=None):
    global mid
    mid += 1
    fname, faddr = frm
    to_s = ", ".join(f"{n} <{a}>" for n, a in to)
    cc_s = ", ".join(f"{n} <{a}>" for n, a in cc)
    name = fname.split()[0]
    body = body_for(kind, name)
    if quote:
        body = f"> {quote}\n\n{body}"
    opener = random.choice(OPENERS if thread_id is None or "Root" not in subject else OPENERS)
    text = opener.format(to=to[0][0].split()[0], body=body, fromn=fname) \
        if random.random() < 0.9 else body
    if attach is not None:
        boundary = f"demo{mid}"
        parts = [
            f"From: {fname} <{faddr}>",
            f"To: {to_s}" + (f"\nCc: {cc_s}" if cc_s else ""),
            f"Subject: {subject}",
            f"Date: {fmt_date(date)}",
            f"Message-ID: <d{mid}@sample.test>",
            f"X-GM-THRID: {4100000000000000 + thread_id}",
            f"X-Gmail-Labels: {labels}",
            f"MIME-Version: 1.0",
            f'Content-Type: multipart/mixed; boundary="{boundary}"',
            "",
            f"--{boundary}",
            "Content-Type: text/plain; charset=utf-8",
            "",
            text,
            "",
            f"--{boundary}",
            f'Content-Type: text/csv; name="{attach}"',
            "Content-Disposition: attachment; filename=" + attach,
            "Content-Transfer-Encoding: base64",
            "",
            "dGVhZGVyLHZhbHVlCm9uZSwxMjMKdHdvLDQ1Ngp0aHJlZSw3ODkK",
            f"--{boundary}--",
        ]
    elif html:
        boundary = f"demo{mid}"
        styled = text.replace("\n\n", "</p><p>")
        parts = [
            f"From: {fname} <{faddr}>",
            f"To: {to_s}" + (f"\nCc: {cc_s}" if cc_s else ""),
            f"Subject: {subject}",
            f"Date: {fmt_date(date)}",
            f"Message-ID: <d{mid}@sample.test>",
            f"X-GM-THRID: {4100000000000000 + thread_id}",
            f"X-Gmail-Labels: {labels}",
            "MIME-Version: 1.0",
            f'Content-Type: multipart/alternative; boundary="{boundary}"',
            "",
            f"--{boundary}",
            "Content-Type: text/plain; charset=utf-8",
            "",
            text,
            "",
            f"--{boundary}",
            "Content-Type: text/html; charset=utf-8",
            "",
            f"<div dir=\"ltr\"><p><b>{subject}</b></p><p>{styled}</p>"
            f"<ul><li>one action item</li><li>a friendly reminder</li></ul>"
            f"<p style=\"color:#3b6ea5\">— {fname}</p></div>",
            f"--{boundary}--",
        ]
    else:
        parts = [
            f"From: {fname} <{faddr}>",
            f"To: {to_s}" + (f"\nCc: {cc_s}" if cc_s else ""),
            f"Subject: {subject}",
            f"Date: {fmt_date(date)}",
            f"Message-ID: <d{mid}@sample.test>",
            f"X-GM-THRID: {4100000000000000 + thread_id}",
            f"X-Gmail-Labels: {labels}",
            "",
            text,
        ]
    msgs.append((date, "\n".join(parts)))

tid = 0
for subject, kind, n in TOPICS:
    date = rand_date()
    if kind == "news":
        frm = random.choice(SERVICES)
        alias_to = PEOPLE[1][0], "tester-alias+news@sample.test"
        labels = "Category Updates" if date > datetime(2025, 6, 1) else "Category Updates"
        if random.random() < 0.5:
            labels = "Inbox,Category Updates"
        add(tid, frm, [alias_to if random.random() < 0.4 else PEOPLE[0]], [], subject, date, kind, labels)
    elif kind == "logistics":
        frm = random.choice(SERVICES)
        attach = "receipt-2025.csv" if random.random() < 0.3 else None
        labels = "Inbox" if random.random() < 0.6 else "Inbox,Starred"
        add(tid, frm, [PEOPLE[0]], [], subject, date, kind, labels, attach=attach)
        if n > 1:
            d2 = date + timedelta(hours=random.randrange(2, 30))
            add(tid, PEOPLE[0], [frm], [], "Re: " + subject, d2, kind,
                "Inbox,Sent", quote=random.choice(QUOTES))
    else:
        cast = random.sample(FRIENDS, k=random.choice([2, 3]))
        pair = [cast[0], random.choice(PEOPLE)]
        frm, to = (pair if random.random() < 0.5 else pair[::-1])
        if kind in ("family", "social"):
            labels = "Inbox,Starred,Category Personal" if random.random() < 0.15 \
                else "Inbox,Category Personal"
        elif kind == "work":
            labels = "Inbox,Work" if random.random() < 0.5 else "Inbox"
        else:
            labels = "Inbox"
        html = random.random() < 0.2
        add(tid, frm, [to], cast[1:2] if random.random() < 0.25 else [],
            subject, date, kind, labels, html=html)
        for i in range(n - 1):
            date = date + timedelta(hours=random.randrange(1, 72),
                                    minutes=random.randrange(60))
            who = random.choice([frm, to, *cast])
            other = to if who is frm else random.choice([frm, to])
            if who is to or who in PEOPLE:
                lbl = "Inbox,Sent"
            elif kind in ("family", "social"):
                lbl = "Inbox,Category Personal"
            else:
                lbl = "Inbox,Starred" if "Starred" in labels and random.random() < 0.3 else "Inbox"
            add(tid, who, [other], [], "Re: " + subject, date, kind, lbl,
                quote=random.choice(QUOTES) if random.random() < 0.5 else None)
    tid += 1

while len(msgs) < 400:
    subject, kind, _ = random.choice(TOPICS)
    date = rand_date()
    frm = random.choice(FRIENDS + SERVICES)
    to = random.choice(PEOPLE)
    if kind in ("family", "social"):
        labels = "Inbox,Category Personal"
    elif kind == "work":
        labels = random.choice(["Inbox,Work", "Work"])
    else:
        labels = random.choice(["Inbox", "Inbox,Starred", "Category Updates", "Inbox"])
    add(tid, frm, [to], [], subject, date, kind, labels,
        html=random.random() < 0.15)
    tid += 1

msgs.sort(key=lambda m: m[0])
out = sys.argv[1] if len(sys.argv) > 1 else \
    __file__.rsplit("/", 1)[0] + "/demo.mbox"
with open(out, "w") as f:
    for date, block in msgs:
        env = block.splitlines()[0].split(": ", 1)[1].split(" <")[1].rstrip(">")
        f.write(f"From {env} {from_line(date)}\n{block}\n\n")
print(f"wrote {len(msgs)} messages (~{round(sum(len(b) for _, b in msgs)/1e6, 2)} MB) -> {out}")
