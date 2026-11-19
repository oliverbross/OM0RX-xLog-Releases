import { RadioTower } from "lucide-react";
import { antennaBands, antennaTypes, equipmentProfile, antennaCollection, selectedAntenna, type AntennaProfile, type StationEquipment } from "./equipmentPreferences";
import "./StationEquipment.css";

export function EquipmentSettings({ value, onChange }: { value: unknown; onChange: (value: StationEquipment) => void }) {
  const profile = equipmentProfile(value);
  const collection = antennaCollection(value), selected = selectedAntenna(value);
  const changeProfile = (next: AntennaProfile) => onChange({ ...collection, profiles: collection.profiles.map(p => p.id === next.id ? { ...next, revision: next.revision + 1 } : p),
    ...(next.id === collection.selectedProfileID ? { antenna: next.antennaType, name: next.name, bands: next.bands } : {}) });
  const addProfile = (source?: AntennaProfile) => {
    const next: AntennaProfile = { ...(source ?? { antennaType: "vertical", bands: [], azimuthOffsetDegrees: 0, archived: false }),
      name: source ? source.name.slice(0, 59) + " copy" : "New antenna", id: crypto.randomUUID(), revision: 0 };
    onChange({ ...collection, profiles: [...collection.profiles, next], selectedProfileID: next.id,
      antenna: next.antennaType, name: next.name, bands: next.bands });
  };
  const select = (p: AntennaProfile) => onChange({ ...collection, selectedProfileID: p.id, name: p.name, antenna: p.antennaType, bands: p.bands });
  const addPair = () => {
    const profiles = collection.profiles.filter(p => !(p.id === "legacy-antenna" && !p.bands.length && p.name === "Station antenna"));
    const preferences = { ...collection.bandPreferences };
    for (const [name, antennaType, bands] of [["Vertical", "vertical", antennaBands.slice(0, 5)], ["Yagi", "yagi", antennaBands.slice(5, 10)]] as const) {
      let antenna = profiles.find(p => !p.archived && p.antennaType === antennaType && bands.every(band => p.bands.includes(band)));
      if (!antenna) { antenna = { id: crypto.randomUUID(), name, antennaType, bands: [...bands], revision: 0, archived: false, azimuthOffsetDegrees: 0 }; profiles.push(antenna); }
      for (const band of bands) preferences[band] = antenna.id;
    }
    const first = profiles.find(p => p.id === preferences["160m"])!;
    onChange({ ...collection, profiles, bandPreferences: preferences, automaticSelection: true,
      selectedProfileID: first.id, name: first.name, antenna: first.antennaType, bands: first.bands });
  };
  return <section className="equipment-settings"><h2>Station equipment</h2>
    <p>Add each physical antenna once, assign its bands, then save. Choose an antenna below to edit it.</p>
    <label>Operator Desk equipment<select aria-label="Operator Desk equipment" value={profile.display} onChange={event => onChange({ ...profile, display: event.target.value as StationEquipment["display"] })}>
      <option value="automatic">Rotator when configured</option><option value="antenna">Fixed antenna</option><option value="hidden">No equipment panel</option>
    </select></label>

    <ul className="equipment-profiles" aria-label="Antennas and assigned bands">{collection.profiles.filter(p => !p.archived).map(p =>
      <li key={p.id}><button type="button" aria-pressed={p.id === selected?.id} onClick={() => select(p)}><strong>{p.name}</strong><span>{antennaTypes[p.antennaType]}</span><small>{p.bands.join(" · ") || "No bands assigned"}</small></button></li>)}</ul>
    <button type="button" disabled={collection.profiles.length > 62} onClick={addPair}>Set up Vertical 160–30m + Yagi 20–10m</button>
    <label>Antenna profile<select aria-label="Antenna profile" value={collection.selectedProfileID ?? ""} onChange={event => {
      const id = event.currentTarget.value;
      if (!id) { onChange({ ...collection, selectedProfileID: null }); return; }
      const p = collection.profiles.find(p => p.id === id && !p.archived);
      if (!p) return;
      onChange({ ...collection, selectedProfileID: p.id, name: p.name, antenna: p.antennaType, bands: p.bands });
    }}><option value="">No selected profile</option>
      {collection.selectedProfileID && !selected && <option value={collection.selectedProfileID} disabled>Unavailable profile ({collection.selectedProfileID})</option>}
      {collection.profiles.filter(p => !p.archived).map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
    <div className="service-actions"><button type="button" disabled={collection.profiles.length >= 64} onClick={() => addProfile()}>Add profile</button>
      <button type="button" disabled={!selected || collection.profiles.length >= 64} onClick={() => selected && addProfile(selected)}>Duplicate profile</button>
      <button type="button" disabled={!selected} onClick={() => selected && onChange({ ...collection, selectedProfileID: null, profiles: collection.profiles.map(p => p.id === selected.id ? { ...p, archived: true, revision: p.revision + 1 } : p) })}>Archive profile</button></div>
    {selected && <fieldset><legend>Edit selected antenna</legend><label>Profile name<input aria-label="Profile name" value={selected.name} maxLength={64} onChange={e => changeProfile({ ...selected, name: e.target.value })} /></label>
      <label>Profile type<select aria-label="Profile type" value={selected.antennaType} onChange={e => changeProfile({ ...selected, antennaType: e.target.value as AntennaProfile["antennaType"] })}>{Object.entries(antennaTypes).map(([id, title]) => <option key={id} value={id}>{title}</option>)}</select></label>
      <fieldset className="equipment-bands"><legend>Profile bands</legend>{antennaBands.map(band => <label key={band}><input type="checkbox" checked={selected.bands.includes(band)} onChange={e => changeProfile({ ...selected, bands: e.target.checked ? [...selected.bands, band] : selected.bands.filter(b => b !== band) })} />{band}</label>)}</fieldset>
      <label>Rotor association<select aria-label="Profile rotor association" value={selected.rotorConfigurationID ?? ""} onChange={e => changeProfile({ ...selected, rotorConfigurationID: e.target.value || undefined })}><option value="">Fixed antenna / no rotor</option><option value="workspace-rotor">Existing workspace rotor configuration</option></select></label>
      <label>Azimuth offset °<input aria-label="Antenna azimuth offset" type="number" min={-360} max={360} value={selected.azimuthOffsetDegrees} onChange={e => changeProfile({ ...selected, azimuthOffsetDegrees: Number(e.target.value) })} /></label></fieldset>}
    <label><input type="checkbox" checked={collection.automaticSelection} onChange={e => onChange({ ...collection, automaticSelection: e.target.checked })} />Automatically select profiles for blank drafts by transmit band</label>
    <fieldset><legend>Antenna used for each transmit band</legend>
      <div className="equipment-band-assignments">{antennaBands.map(band => {
        const matches = collection.profiles.filter(p => !p.archived && p.bands.includes(band));
        return <label key={band}>{band}<select aria-label={`${band} preferred antenna`} value={collection.bandPreferences[band] ?? (matches.length === 1 ? matches[0].id : "")} onChange={e => {
          const preferences = { ...collection.bandPreferences };
          if (e.target.value) preferences[band] = e.target.value; else delete preferences[band];
          onChange({ ...collection, bandPreferences: preferences, profiles: collection.profiles.map(p => p.id === e.target.value && !p.bands.includes(band) ? { ...p, bands: [...p.bands, band], revision: p.revision + 1 } : p) });
        }}><option value="">{matches.length ? "Choose when logging" : "Unassigned"}</option>{collection.profiles.filter(p => !p.archived).map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>;
      })}</div></fieldset>
    <button type="submit" className="primary">Save station equipment</button>
    <p>Profile selection changes future-contact metadata only. It sends no switch, connect or movement command. Saved contacts retain their snapshots.</p>
  </section>;
}

function AntennaDiagram({ type }: { type: StationEquipment["antenna"] }) {
  // A schematic, not a claimed drawing of the operator's physical installation.
  return <svg className="equipment-diagram" viewBox="0 0 120 80" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    {type === "vertical" ? <><path d="M60 8V63M60 63L25 73M60 63L95 73M60 63V76" /><path className="equipment-secondary" d="M55 20h10m-10 13h10m-10 13h10" /></> :
      type === "wire" ? <><path d="M15 65V20M105 65V12M15 20Q60 45 105 12" /><path className="equipment-secondary" d="M62 30v38" /></> :
      type === "dipole" ? <><path d="M12 48L60 17l48 31M60 17v54" /><path className="equipment-secondary" d="M55 68h10" /></> :
      type === "yagi" ? <><path d="M60 12v60M27 22h66M33 35h54M39 48h42" /><path className="equipment-secondary" d="M52 74h16" /></> :
      type === "loop" ? <><circle cx="60" cy="33" r="25" /><path d="M60 58v15M45 74h30" /></> : <><path d="M60 18v54M36 8l24 24L84 8M45 74h30" /></>}
  </svg>;
}

export function DeskAntenna({ profile, taskOnly = false }: { profile: StationEquipment; taskOnly?: boolean }) {
  const selected = selectedAntenna(profile);
  if (selected) profile = { ...profile, antenna: selected.antennaType, name: selected.name, bands: selected.bands };
  return <section className="desk-panel desk-antenna" aria-label="Configured antenna">{!taskOnly && <h2><RadioTower size={15} aria-hidden="true" />Antenna</h2>}
    <div className="desk-antenna-body"><AntennaDiagram type={profile.antenna} /><div><strong>{profile.name || antennaTypes[profile.antenna]}</strong>{profile.name && <span>{antennaTypes[profile.antenna]}</span>}<small>Software profile · no switch telemetry</small></div></div>
    <div className="desk-antenna-bands" aria-label="Configured antenna bands">{profile.bands.length ? profile.bands.map(band => <span key={band}>{band}</span>) : <small>No bands specified</small>}</div>
  </section>;
}
