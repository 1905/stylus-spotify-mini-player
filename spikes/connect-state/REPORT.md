# connect-state spike: commands from C (HTTP only) to T (Spirc)

| command | endpoint | HTTP | body start | expected | seen | result |
|---|---|---|---|---|---|---|
| transfer | `POST /connect-state/v1/connect/transfer/from/{C}/to/{T} (SpClient::transfer)` | 2xx | {   "ack_id": "ByrtQvqFUWNyVBobXkt4mIiejlg" } | T loads a track (it was idle) | active=true paused pos=0s track=6aiKIFjP shuffle=false repeat=off vol=13107 | PASS |
| play context | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "1g8STzbYsUb2fv8jzNw95W-Yf7s" } | plays the known track (context not visible in player events) | active=true playing pos=0s track=4uLU6hMC shuffle=false repeat=off vol=13107 (after 3.5 s) [body 1] | PASS |
| pause | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "-8QE2W0pcZcQY-KYTgHwOMvj4vM" } | paused | active=true paused pos=0s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| resume | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "xvi4EQvzG226iB5qmEBPYaRJxQs" } | playing | active=true playing pos=2s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| seek_to | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "lWYW3_mEr1t7JPC0aKDOit2D7SU" } | position 60-64 s | active=true playing pos=61s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| skip_next | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "x9LrT46mzQpmdvr0_7IDW2F_a_s" } | another track | active=true playing pos=1s track=6aiKIFjP shuffle=false repeat=off vol=13107 [body 1] | PASS |
| skip_prev | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "0tcVMWsmYRByv031YBVR00KG3SI" } | previous track, or restart when >3 s in | active=true playing pos=2s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| set_shuffling_context true | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "vGB7HhftQPYDeZGpAO6DdND8RQc" } | shuffling_context = true | active=true playing pos=7s track=4uLU6hMC shuffle=true repeat=off vol=13107 [body 1] | PASS |
| set_shuffling_context false | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "-yhzQp5ix5e7DqHgqWZAXt7MqY4" } | shuffling_context = false | active=true playing pos=10s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| set_repeating_context true | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "q7KvfvpzfldxARPVBE86DEGT-Pk" } | repeating_context = true | active=true playing pos=12s track=4uLU6hMC shuffle=false repeat=context vol=13107 [body 1] | PASS |
| set_repeating_track true | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "zjpeZeZm8V6xkvFUBBTkX6wGjBQ" } | repeating_track = true | active=true playing pos=14s track=4uLU6hMC shuffle=false repeat=track vol=13107 [body 1] | PASS |
| set_repeating_track false | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "dTZ-LvmUa15KE1I6-j7neROgP5Y" } | repeating_track = false | active=true playing pos=17s track=4uLU6hMC shuffle=false repeat=context vol=13107 [body 1] | PASS |
| set_repeating_context false | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "_7y6ueU2M7qCJfIMLI6k_mtwBk4" } | repeating_context = false | active=true playing pos=19s track=4uLU6hMC shuffle=false repeat=off vol=13107 [body 1] | PASS |
| volume | `PUT /connect-state/v1/connect/volume/from/{C}/to/{T}` | 200 | {   "ack_id": "ZXpizhhcR_zpyKAJLdhNssmnHPk" } | T volume 32768 (50 %) | active=true playing pos=21s track=4uLU6hMC shuffle=false repeat=off vol=32768 | PASS |
| play uris | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "prpD9JSAb0vcw-_PP82YZypQU6A" } | plays the 2nd uri of the list | active=true playing pos=1s track=6aiKIFjP shuffle=false repeat=off vol=32768 [body 1] | PASS |
| play context (minimal body, 2nd time) | `POST /connect-state/v1/player/command/from/{C}/to/{T}` | 200 | {   "ack_id": "XFNmxvWou1cELz14RzxSya9uh3Y" } | plays the known track | active=true playing pos=1s track=4uLU6hMC shuffle=false repeat=off vol=32768 | PASS |

## Bodies (the one that passed, else the last one tried)

- transfer (PASS): `{"transfer_options":{"restore_paused":"restore"}}`
- play context (PASS): `{"command":{"context":{"uri":"spotify:album:6N9PS4QXF1D0OWPk0Sxtb4","url":"context://spotify:album:6N9PS4QXF1D0OWPk0Sxtb4"},"endpoint":"play","options":{"skip_to":{"track_uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"}}}}`
- pause (PASS): `{"command":{"endpoint":"pause"}}`
- resume (PASS): `{"command":{"endpoint":"resume"}}`
- seek_to (PASS): `{"command":{"endpoint":"seek_to","value":60000}}`
- skip_next (PASS): `{"command":{"endpoint":"skip_next"}}`
- skip_prev (PASS): `{"command":{"endpoint":"skip_prev"}}`
- set_shuffling_context true (PASS): `{"command":{"endpoint":"set_shuffling_context","value":true}}`
- set_shuffling_context false (PASS): `{"command":{"endpoint":"set_shuffling_context","value":false}}`
- set_repeating_context true (PASS): `{"command":{"endpoint":"set_repeating_context","value":true}}`
- set_repeating_track true (PASS): `{"command":{"endpoint":"set_repeating_track","value":true}}`
- set_repeating_track false (PASS): `{"command":{"endpoint":"set_repeating_track","value":false}}`
- set_repeating_context false (PASS): `{"command":{"endpoint":"set_repeating_context","value":false}}`
- volume (PASS): `{"volume":32768}`
- play uris (PASS): `{"command":{"context":{"pages":[{"tracks":[{"uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"},{"uri":"spotify:track:6aiKIFjPwa3UvDCD5ecoJj"}]}]},"endpoint":"play","options":{"skip_to":{"track_index":1}}}}`
- play context (minimal body, 2nd time) (PASS): `{"command":{"context":{"uri":"spotify:album:6N9PS4QXF1D0OWPk0Sxtb4","url":"context://spotify:album:6N9PS4QXF1D0OWPk0Sxtb4"},"endpoint":"play","options":{"skip_to":{"track_uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"}}}}`
