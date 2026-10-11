# Clean-room policy

LoopCraft reproduces a *workflow*: clip launching, scenes, warping, an arrangement timeline. Workflows and general ideas aren't owned by anyone. Code, assets and trade dress are. To keep the project safe for everyone who uses and contributes to it:

**Never** contribute anything derived from Ableton's software: source code, decompiled or disassembled binaries, bundled samples, presets, Packs, device graphics, icons, fonts, documentation text, or the Max for Live runtime. Don't paste from Ableton's manuals.

**Do** work from public, general knowledge: textbook DSP, published papers, open specifications (CLAP, MIDI, WAV), your own experience as a user describing *what* a feature does, and permissively licensed open-source code (with attribution).

**Algorithms used so far and where they come from:**
- PolyBLEP oscillators (Välimäki et al.)
- TPT state-variable filter (Zavalishin, *The Art of VA Filter Design*)
- RBJ biquad cookbook (Robert Bristow-Johnson)
- Freeverb topology (Jezar at Dreampoint, public domain)
- WSOLA time-stretching (Verhelst & Roelands, 1993)

**Names and look.** LoopCraft uses its own name, its own device names (Poly Synth, not "Analog"), and its own visual design. It may describe itself as an alternative to Ableton Live for clarity, alongside the not-affiliated notice in the README. "Ableton" and "Live" are trademarks of Ableton AG.

**Patents.** Large vendors hold audio patents. Many core techniques here are decades old and widely implemented, but this isn't legal advice; if you're implementing something you know is patented, raise it in an issue first.

**File compatibility.** Reading `.als` project files may be added later. It must be done only from the structure of files users created themselves, never from Ableton's code.
