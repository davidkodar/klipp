# First Cut

First Cut makes a rough cut of someone talking to camera in one step: the
pauses, filler words and retakes come out, every other piece of a shot is
pushed in, and captions go on. It is one undo step.

## Where to find it

- **Edit → First Cut…**, always.
- A **First Cut** button on the timeline's toolbar. It shows only while a clip
  with speech is on the timeline. Preferences → Editing → **First Cut button**
  hides it.

## What it does

The dialog lists the steps. Each can be turned off:

- **Take out pauses.** The pauses measured in the sound are cut, leaving a
  little at each end. **Gentle** cuts pauses of a second or more, **Normal** of
  0.6 s or more, and **Tight** of 0.35 s or more.
- **Take out filler words.** Hesitation sounds ("um", "uh") in the transcript,
  in its language.
- **Keep the last take of a line said twice.** When a line is said again within
  three sentences, the earlier take and anything said between are cut.
- **Punch in on every other piece of a shot.** The cuts above make jump cuts;
  pushing every other piece in a little makes them read as cuts. Where the
  footage has been analysed for people, it reframes on the speaker.
- **Add captions**, from the transcript, when the sequence has none. Your own
  captions are never replaced.
- **Music bed.** A sound file you choose goes under the speech on a new audio
  track, as long as the cut, and is lowered while someone speaks. None is
  added unless you choose one.

The cuts are taken from every unlocked track, so picture and sound stay
together.

Pauses, filler words and retakes are found in the transcript and the measured
sound. The dialog names any clips with speech that aren't transcribed yet, and
**Transcribe them** does that first.

## Taking one change back

After a First Cut, the **First Cut changes** panel lists every change it made,
with its time: each pause, filler word and retake, and the punch-in, captions
and music. Click a change to go to it on the timeline.

Untick a change to take just that one back; tick it again to put it back. Each
is its own undo step. The cut is made again without the changes you untick,
from the timeline as it was before First Cut, so the rest stays exactly as it
was.

Once you edit the timeline after First Cut, the list can't be changed any more,
since swapping the cut would lose your edit. Undo still takes First Cut back.
The list is kept until Klipp closes; it isn't saved with the project.

## With an assistant

The assistant's `first_cut` operation makes the same cut from the same
choices. Its reply lists each change, numbered, and `exclude` leaves changes
out, as unticking them does.
