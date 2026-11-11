import json
import tkinter as tk
from pathlib import Path
from tkinter import messagebox, ttk

SAVED = Path.home() / ".twilight.json"
MODES = {"Auto": 0, "UDP only": 1, "TCP only": 2}


def load():
    try:
        return json.loads(SAVED.read_text())
    except (OSError, ValueError):
        return {}


def ask(error=None):
    saved = load()
    root = tk.Tk()
    root.title("Twilight")
    root.resizable(False, False)
    frame = ttk.Frame(root, padding=16)
    frame.grid()
    fields = {
        "host": tk.StringVar(value=saved.get("host", "")),
        "pin": tk.StringVar(),
        "mode": tk.StringVar(value=saved.get("mode", "Auto")),
        "stripes": tk.IntVar(value=saved.get("stripes", 4)),
        "fps": tk.IntVar(value=saved.get("fps", 30)),
    }
    rows = [
        ("Host IP", ttk.Entry(frame, textvariable=fields["host"], width=24)),
        ("PIN", ttk.Entry(frame, textvariable=fields["pin"], width=24)),
        ("Transport", ttk.Combobox(frame, textvariable=fields["mode"], values=list(MODES), state="readonly", width=21)),
        ("TCP stripes", ttk.Spinbox(frame, textvariable=fields["stripes"], from_=1, to=8, width=22)),
        ("FPS", ttk.Spinbox(frame, textvariable=fields["fps"], from_=5, to=120, width=22)),
    ]
    for row, (label, widget) in enumerate(rows):
        ttk.Label(frame, text=label).grid(row=row, column=0, sticky="w", pady=3, padx=(0, 12))
        widget.grid(row=row, column=1, pady=3)
    result = {}

    def connect(_=None):
        try:
            values = {k: v.get() for k, v in fields.items()}
        except tk.TclError:
            return messagebox.showerror("Twilight", "Stripes and FPS must be numbers", parent=root)
        if not values["host"].strip() or not values["pin"].strip():
            return messagebox.showerror("Twilight", "Enter the host IP and the PIN it shows", parent=root)
        result.update(values)
        try:
            SAVED.write_text(json.dumps({k: values[k] for k in ("host", "mode", "stripes", "fps")}))
        except OSError:
            pass
        root.destroy()

    ttk.Button(frame, text="Connect", command=connect).grid(row=len(rows), column=0, columnspan=2, pady=(12, 0), sticky="ew")
    root.bind("<Return>", connect)
    rows[0 if not fields["host"].get() else 1][1].focus_set()
    if error:
        root.after(100, lambda: messagebox.showerror("Twilight", error, parent=root))
    root.mainloop()
    if not result:
        return None
    return {"host": result["host"].strip(), "pin": result["pin"].strip(), "mode": MODES[result["mode"]],
            "stripes": max(1, min(8, result["stripes"])), "fps": max(5, min(120, result["fps"]))}
