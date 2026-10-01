extends RefCounted

const FRAMES = [
	{"id":"kestrel","name":"Kestrel","role":"INTERCEPTOR","body":"Salvage and repair cells recharge your dash.","detail":"100 hull / rapid recovery","health":100.0,"armor":0.0,"color":"69f4de","symbol":7},
	{"id":"bastion","name":"Bastion","role":"HEAVY ESCORT","body":"Reinforced armour absorbs incoming damage.","detail":"140 hull / 10% resistance","health":140.0,"armor":0.1,"color":"ffd080","symbol":8},
	{"id":"relay","name":"Relay","role":"FIELD ENGINEER","body":"Launch with an escort drone already fitted.","detail":"95 hull / autonomous escort","health":95.0,"armor":0.0,"color":"69d7ff","symbol":9}
]
const ACTS = [
	{"name":"Breakwater Dock","short":"BREAKWATER","color":"69f4de","brief":"Clear a route out of the salvage yards."},
	{"name":"Aperture Relay","short":"APERTURE","color":"69d7ff","brief":"Restore the relay before the blockade closes."},
	{"name":"Ash Foundry","short":"ASH FOUNDRY","color":"ffb16b","brief":"Cross the foundry and silence the Warden."}
]
# Fixed encounter compositions give each stop a deliberate job and threat mix.
const ENCOUNTERS = [
	{"name":"Departure clearance","act":0,"objective":"clear","quota":10,"mix":["seeker"],"brief":"Mara: Local patrols have the exit. Break their formation."},
	{"name":"Loose cargo","act":0,"objective":"salvage","quota":12,"mix":["seeker","spitter","seeker"],"brief":"Mara: Three sealed caches are on the deck. Recover them before lockdown."},
	{"name":"Dockside hold","act":0,"objective":"defend","quota":15,"mix":["seeker","brute","spitter"],"brief":"Mara: Keep those cutters away from the dock's reactor."},
	{"name":"The Gatekeeper","act":0,"objective":"boss","quota":1,"mix":["gatekeeper"],"boss":"gatekeeper","boss_hp":2200.0,"brief":"Mara: The Gatekeeper owns this channel. Watch the gaps in its volleys."},
	{"name":"Signal intercept","act":1,"objective":"clear","quota":16,"mix":["skirmisher","spitter","sentinel","seeker"],"brief":"Mara: Interceptors are moving between the relay pylons. Stay mobile."},
	{"name":"Dead air","act":1,"objective":"hold","duration":38.0,"quota":18,"mix":["mine","skirmisher","spitter","sentinel"],"brief":"Mara: The relay is aligning. Hold the channel until it locks."},
	{"name":"Black-box recovery","act":1,"objective":"salvage","quota":19,"mix":["sentinel","skirmisher","bomber","spitter"],"brief":"Mara: The recorders survived. You have two minutes before the channel locks."},
	{"name":"The Carrier","act":1,"objective":"boss","quota":1,"mix":["carrier"],"boss":"carrier","boss_hp":6500.0,"brief":"Mara: The Carrier is deploying fresh patrols. Cut through its escort."},
	{"name":"Hot approach","act":2,"objective":"clear","quota":22,"mix":["brute","bomber","sentinel","skirmisher"],"brief":"Mara: Foundry vents are live. Cross the marked lanes before they flare."},
	{"name":"Containment breach","act":2,"objective":"defend","quota":24,"mix":["brute","seeker","bomber","sentinel"],"brief":"Mara: That core powers the route home. Keep it intact."},
	{"name":"Last transmission","act":2,"objective":"hold","duration":46.0,"quota":26,"mix":["skirmisher","sentinel","mine","bomber","spitter"],"brief":"Mara: We're sending your exit codes. Buy the relay a little more time."},
	{"name":"The Warden","act":2,"objective":"boss","quota":1,"mix":["warden"],"boss":"warden","boss_hp":15000.0,"brief":"Mara: One contact remains. Break the Warden and the route is yours."}
]
const ENEMIES = {
	"seeker":{"name":"Cutter","hp":42.0,"speed":2.9,"radius":0.5,"bounty":20,"note":"A pursuit craft. Its direct approach leaves it open to wing shots."},
	"spitter":{"name":"Spitter","hp":70.0,"speed":2.7,"radius":0.5,"bounty":25,"note":"Keeps firing distance. Close the gap or cut across its aim."},
	"brute":{"name":"Bulwark","hp":150.0,"speed":1.8,"radius":0.85,"bounty":35,"note":"Armoured pressure. A piercing round can catch the patrol behind it."},
	"skirmisher":{"name":"Interceptor","hp":62.0,"speed":3.3,"radius":0.55,"bounty":30,"note":"Strafes before a short rush. Its warning lights announce the turn."},
	"sentinel":{"name":"Sentinel","hp":130.0,"speed":1.6,"radius":0.8,"bounty":40,"note":"A cycling frontal shield. Flank it, wait for its opening or use Arc."},
	"bomber":{"name":"Bombardier","hp":100.0,"speed":2.1,"radius":0.65,"bounty":35,"note":"Marks your position before a strike. Leave the circle, then return fire."},
	"mine":{"name":"Drift mine","hp":26.0,"speed":0.4,"radius":0.45,"bounty":15,"note":"Arms at close range. Shoot it from a distance or dash clear."},
	"gatekeeper":{"name":"Gatekeeper","hp":2200.0,"speed":0.7,"radius":1.6,"bounty":500,"note":"Rotating volleys and crossing lanes. The centre is rarely the safest path."},
	"carrier":{"name":"Carrier","hp":6500.0,"speed":1.0,"radius":1.8,"bounty":750,"note":"Deploys an escort while retreating. Clear its helpers to keep room to move."},
	"warden":{"name":"Warden","hp":4800.0,"speed":1.45,"radius":1.6,"bounty":1200,"note":"Containment, pursuit, overload. Each phase takes away a little more safe space."}
}
const BOSSES = ["gatekeeper","carrier","warden"]

static func encounter(number: int) -> Dictionary:
	return ENCOUNTERS[clampi(number-1,0,ENCOUNTERS.size()-1)].duplicate(true)

static func frame(id: String) -> Dictionary:
	for entry in FRAMES:
		if entry.id==id: return entry.duplicate()
	return FRAMES[0].duplicate()
