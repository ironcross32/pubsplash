# Stream scheduling

A user may opt to schedule a stream to go live at a specific time. There are two ways of scheduling. The most simple of these is that the user selects the time, hits the schedule button, and at that time, the stream goes live using whatever is their active scene. The more advanced method works as follows.

The user selects a time which represents the time their actual content will begin. They also select a negative offset which can be in either seconds or minutes. Then they select two scenes. The idea is that Pubsplash will connect at the start time minus the offset and begin streaming the first scene, then at the start time, it'll automatically switch to the second scene. The first scene might contain nothing but music, while the second will presumably contain their mic, and whatever applications or other sources they'd want.

## Workflow

1. User goes to File > Schedule Stream
2. Focus lands on a radio button group where they will choose between either simple or advanced
3. In simple mode, they select a time using TimePickerCtrl and hit OK which schedules the Stream
4. In advanced mode, as outlined above, they select two times. Call them "Pre-stream" and "Start of stream". Pre-stream is when Pubsplash actually connects, Start stream is when it switches
5. They choose two scenes, label them the same as the two times
6. They press OK and the stream is scheduled.

## What happens next

The first row of the status list that normally shows whether Pubsplash is streaming, recording or is idle will now show that a stream is scheduled, and there'll be a countdown until Pubsplash will connect. When Pubsplash connects and the server accepts the stream, that line will then either:
- Behave as if the user simply started a non-scheduled stream IF they scheduled with the simple mode
- Show a countdown to the scene switch IF they scheduled with the advanced mode
In advanced mode, when the scene switches, the status line will resume normal behavior.

On the home tab, the start streaming button will change to "Cancel stream" if a scheduled stream is waiting to go live. The record button should also be made unavailable so it cannot cause an issue. This must also affect keybinds, so that a start / stop recording binding can't also be triggered when a schedule is in the cue.

Once a scheduled stream connects to Audiopub, the button will change to stop streaming as it always has. Once that button is clicked, or the keybinding is pressed, the stream stops and any remaining scheduled tasks are discarded.