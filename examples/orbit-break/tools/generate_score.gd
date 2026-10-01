extends SceneTree

const RATE = 16000
const BEAT = 0.6
const LEAD = [62,69,65,67,58,65,62,69,65,72,69,67,60,67,64,69]
const ROOTS = [38,34,41,36]
const CHORDS = [[50,53,57],[46,50,53],[53,57,60],[48,52,55]]

func hz(note: int) -> float:
	return 440.0*pow(2.0,(note-69)/12.0)

func _initialize() -> void:
	var output = OS.get_environment("ORBIT_SCORE_OUTPUT")
	if output.is_empty(): quit(2); return
	DirAccess.make_dir_recursive_absolute(output)
	var names = ["breakwater","aperture","foundry"]
	for act in range(3):
		var notes: Array[float] = []
		for note in LEAD: notes.append(hz(note+(12 if act==1 else 0)))
		var roots: Array[float] = []
		for note in ROOTS: roots.append(hz(note))
		var chords: Array[Array] = []
		for chord in CHORDS: chords.append([hz(chord[0]),hz(chord[1]),hz(chord[2])])
		var count = roundi(RATE*BEAT*16)
		var bytes = PackedByteArray()
		bytes.resize(count*2)
		for i in range(count):
			var time = float(i)/RATE
			var step = mini(15,int(time/BEAT))
			var chord = int(step/4)
			var local = fmod(time,BEAT)
			var attack = minf(local/0.015,1.0)*exp(-local*(6.5 if act==1 else 5.0))
			var lead = sin(TAU*notes[step]*time)*0.075*attack
			lead += sin(TAU*notes[step]*2.0*time)*0.015*attack
			var pad = 0.0
			for frequency in chords[chord]: pad += sin(TAU*frequency*time)*0.017
			var bass = sin(TAU*roots[chord]*time)*0.055*(0.75+0.25*cos(TAU*local/BEAT))
			var percussion = 0.0
			if act!=1:
				var kick = sin(TAU*(46.0*local+18.0*(1.0-exp(-local*18.0))/18.0))*exp(-local*22.0)
				percussion += kick*(0.12 if act==2 else 0.065)
				var half = fmod(time,BEAT*0.5)
				percussion += sin(i*1.77)*sin(i*0.73)*exp(-half*65.0)*(0.025 if act==2 else 0.013)
			var edge = maxf(0,minf(minf(time/0.04,(float(count)/RATE-time)/0.06),1.0))
			var sample = clampi(roundi((lead+pad+bass+percussion)*edge*29000.0),-32767,32767)
			bytes.encode_s16(i*2,sample)
		var stream = AudioStreamWAV.new()
		stream.format = AudioStreamWAV.FORMAT_16_BITS
		stream.mix_rate = RATE
		stream.data = bytes
		if stream.save_to_wav(output.path_join(names[act]+".wav"))!=OK: quit(3); return
	quit()
