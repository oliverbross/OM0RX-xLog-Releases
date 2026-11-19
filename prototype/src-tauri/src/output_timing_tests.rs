use super::*;

fn fixture(ft4:bool)->(Request,Calibration,Policy,ClockSample){
    let format=Format {rate:12_000,channels:2,encoding:Encoding::F32Le};
    let binding=Binding {calibration:1,clock:2,device:3,route:4,policy:5,reservation:6,owner_context_sha256:[7;32]};
    let (period,frames,first,end,close,setup)=if ft4 {(7_500_000_000,90_000,3600,64_080,6_000_000_000,200_000_000)}
        else {(15_000_000_000,180_000,6000,157_680,14_000_000_000,400_000_000)};
    let span=Span {source:SpanSource::SyntheticFixture,body_sha256:[8;32],format,period_ns:period,
        full_frames:frames,signal_first:first,signal_end:end};
    let request=Request {operation:9,binding:binding.clone(),body_sha256:[8;32],format,
        channel_map:vec![1],slot_utc_ns:30*SECOND,span};
    let calibration=Calibration {binding,format,channel_map:vec![1],utc_anchor_ns:0,mono_anchor_ns:SECOND,
        output_anchor_frame:0,output_anchor_mono_ns:SECOND,measured_at_mono_ns:SECOND,
        valid_until_mono_ns:100*SECOND,uncertainty_ns:0,drift_ppb:0};
    let policy=Policy {epoch:5,max_calibration_age_ns:60*SECOND,max_uncertainty_ns:1_000_000,
        max_clock_step_ns:100_000,max_horizon_ns:60*SECOND,max_alignment_ns:100_000,
        required_enqueue_lead_ns:100_000_000,ptt_setup_ns:setup,ptt_release_ns:200_000_000,
        ptt_window_open_ns:0,ptt_window_close_ns:close,max_ptt_duration_ns:close};
    (request,calibration,policy,ClockSample {epoch:2,utc_ns:28*SECOND,mono_ns:29*SECOND})
}

#[test]
fn ft8_plan_separates_lead_signal_and_padding_from_ptt(){
    let (r,c,p,n)=fixture(false);let result=plan(&r,&c,&p,n).unwrap();
    assert_eq!(result.slot_mono_ns,31*SECOND);assert_eq!(result.first_frame,360_000);
    assert_eq!(result.end_frame,540_000);assert_eq!(result.signal_first_frame,366_000);
    assert_eq!(result.signal_end_frame,517_680);assert_eq!(result.signal_first_ns,31_500_000_000);
    assert_eq!(result.signal_end_ns,44_140_000_000);assert_eq!(result.ptt_on_ns,31_100_000_000);
    assert_eq!(result.ptt_off_ns,44_340_000_000);assert_eq!(result.end_presentation_ns,46*SECOND);
    assert_eq!(result.commit_deadline_ns,30_900_000_000);assert!(result.ptt_off_ns<result.end_presentation_ns);
}
#[test]
fn ft4_plan_has_its_own_explicit_signal_window(){
    let (r,c,p,n)=fixture(true);let result=plan(&r,&c,&p,n).unwrap();
    assert_eq!(result.end_frame-result.first_frame,90_000);
    assert_eq!(result.signal_first_ns,31_300_000_000);assert_eq!(result.signal_end_ns,36_340_000_000);
    assert_eq!(result.ptt_off_ns,36_540_000_000);assert_eq!(result.end_presentation_ns,38_500_000_000);
}
#[test]
fn every_epoch_and_owner_context_binding_change_retires_plan(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    for choice in 0..8 {
        let mut changed=r.clone();match choice {0=>changed.binding.calibration+=1,1=>changed.binding.clock+=1,
            2=>changed.binding.device+=1,3=>changed.binding.route+=1,4=>changed.binding.policy+=1,
            5=>changed.binding.reservation+=1,6=>changed.binding.owner_context_sha256[0]^=1,_=>changed.operation+=1}
        assert_eq!(original.revalidate_admission(&changed,&c,&p,n),Err(Error::Binding));
    }
}
#[test]
fn refreshed_calibration_never_rebases_or_extends_existing_plan(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    let mut changed=c.clone();changed.valid_until_mono_ns+=SECOND;
    assert_eq!(original.revalidate_admission(&r,&changed,&p,n),Err(Error::Binding));
    let mut changed=c.clone();changed.output_anchor_frame+=1;
    assert_eq!(original.revalidate_admission(&r,&changed,&p,n),Err(Error::Binding));
}
#[test]
fn fresh_matching_observation_can_revalidate_without_changing_schedule(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    original.revalidate_admission(&r,&c,&p,ClockSample {utc_ns:n.utc_ns+SECOND,mono_ns:n.mono_ns+SECOND,..n}).unwrap();
    let next=plan(&r,&c,&p,ClockSample {utc_ns:n.utc_ns+SECOND,mono_ns:n.mono_ns+SECOND,..n}).unwrap();
    assert_eq!(original,next);
}
#[test]
fn whole_body_and_span_certificate_must_agree(){
    let (mut r,c,p,n)=fixture(false);r.span.body_sha256[0]^=1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Span));
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();let mut changed=r.clone();
    changed.body_sha256[0]^=1;changed.span.body_sha256=changed.body_sha256;
    assert_eq!(original.revalidate_admission(&changed,&c,&p,n),Err(Error::Binding));
}
#[test]
fn final_format_mismatch_is_refused_without_conversion(){
    for change in 0..4 {let (r,mut c,p,n)=fixture(false);match change {0=>c.format.rate=48_000,
        1=>c.format.channels=1,2=>c.format.encoding=Encoding::I16Le,_=>c.format.rate=0}
        assert_eq!(plan(&r,&c,&p,n),Err(Error::Format));}
    let (mut r,c,p,n)=fixture(false);r.span.format.rate=48_000;assert_eq!(plan(&r,&c,&p,n),Err(Error::Format));
}
#[test]
fn channel_map_has_no_duplicate_out_of_range_or_implicit_default(){
    for channels in [vec![],vec![0,0],vec![2],vec![0,1,0]] {
        let (mut r,c,p,n)=fixture(false);r.channel_map=channels;assert_eq!(plan(&r,&c,&p,n),Err(Error::Format));
    }
}
#[test]
fn future_backward_or_stale_measurements_are_unavailable(){
    let (r,c,p,n)=fixture(false);
    for now in [ClockSample {mono_ns:0,..n},ClockSample {mono_ns:c.valid_until_mono_ns,..n}] {
        assert_eq!(plan(&r,&c,&p,now),Err(Error::Expired));
    }
    let mut p=p;p.max_calibration_age_ns=SECOND;assert_eq!(plan(&r,&c,&p,n),Err(Error::Expired));
}
#[test]
fn either_direction_of_utc_discontinuity_is_rejected(){
    let (r,c,p,n)=fixture(false);
    for utc in [n.utc_ns-SECOND,n.utc_ns+SECOND] {
        assert_eq!(plan(&r,&c,&p,ClockSample {utc_ns:utc,..n}),Err(Error::ClockChanged));
    }
    assert_eq!(plan(&r,&c,&p,ClockSample {epoch:n.epoch+1,..n}),Err(Error::Binding));
}
#[test]
fn clock_step_tolerance_has_an_exact_boundary(){
    let (r,c,p,n)=fixture(false);
    plan(&r,&c,&p,ClockSample {utc_ns:n.utc_ns+p.max_clock_step_ns,..n}).unwrap();
    assert_eq!(plan(&r,&c,&p,ClockSample {utc_ns:n.utc_ns+p.max_clock_step_ns+1,..n}),Err(Error::ClockChanged));
}
#[test]
fn uncertainty_includes_drift_through_end_of_full_presentation(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=100;c.drift_ppb=10;
    let result=plan(&r,&c,&p,n).unwrap();assert_eq!(result.uncertainty_ns,551);
    p.max_uncertainty_ns=550;assert_eq!(plan(&r,&c,&p,n),Err(Error::Uncertain));
    p.max_uncertainty_ns=551;plan(&r,&c,&p,n).unwrap();
}
#[test]
fn calibration_must_cover_full_presentation_and_horizon(){
    let (r,mut c,p,n)=fixture(false);c.valid_until_mono_ns=46*SECOND-1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Expired));
    let (r,c,mut p,n)=fixture(false);p.max_horizon_ns=16*SECOND;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Expired));
}
#[test]
fn late_commit_and_past_slot_never_become_catch_up_frames(){
    let (r,c,p,n)=fixture(false);let result=plan(&r,&c,&p,n).unwrap();
    let boundary=result.commit_deadline_ns;
    plan(&r,&c,&p,ClockSample {mono_ns:boundary,utc_ns:boundary-SECOND,..n}).unwrap();
    assert_eq!(plan(&r,&c,&p,ClockSample {mono_ns:boundary+1,utc_ns:boundary+1-SECOND,..n}),Err(Error::Late));
    assert_eq!(plan(&r,&c,&p,ClockSample {mono_ns:32*SECOND,utc_ns:31*SECOND,..n}),Err(Error::Late));
}
#[test]
fn frame_rounding_is_explicit_and_alignment_error_is_bounded(){
    let (r,mut c,mut p,n)=fixture(false);c.output_anchor_mono_ns+=1;
    let result=plan(&r,&c,&p,n).unwrap();assert_eq!(result.first_presentation_ns,result.slot_mono_ns+1);
    p.max_alignment_ns=0;assert_eq!(plan(&r,&c,&p,n),Err(Error::Alignment));
}
#[test]
fn fractional_rate_frame_offsets_round_up_with_checked_integer_math(){
    let (mut r,mut c,p,n)=fixture(false);r.format.rate=44_100;c.format=r.format;r.span.format=r.format;
    r.span.full_frames=661_500;r.span.signal_first=22_050;r.span.signal_end=579_474;
    c.output_anchor_mono_ns+=10_000;
    let result=plan(&r,&c,&p,n).unwrap();
    assert!(result.first_presentation_ns>=result.slot_mono_ns);
    assert!(result.first_presentation_ns-result.slot_mono_ns<=22_676);
    assert_eq!(result.end_frame-result.first_frame,661_500);
}
#[test]
fn span_order_period_geometry_and_slot_alignment_are_exact(){
    for change in 0..6 {let (mut r,c,p,n)=fixture(false);match change {
        0=>r.span.signal_first=r.span.signal_end,1=>r.span.signal_end=r.span.full_frames+1,
        2=>r.span.full_frames-=1,3=>r.span.period_ns=0,4=>r.slot_utc_ns+=1,_=>r.span.period_ns=SECOND}
        assert_eq!(plan(&r,&c,&p,n),Err(Error::Span));}
}
#[test]
fn ptt_window_rejects_setup_release_or_duration_outside_policy(){
    for change in 0..4 {let (r,c,mut p,n)=fixture(false);match change {
        0=>p.ptt_setup_ns=600_000_000,1=>p.ptt_release_ns=2*SECOND,
        2=>p.max_ptt_duration_ns=SECOND,_=>p.ptt_window_open_ns=200_000_000}
        assert_eq!(plan(&r,&c,&p,n),Err(Error::PttWindow));}
}
#[test]
fn ptt_is_bounded_by_signal_certificate_not_full_buffer_padding(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    let mut changed=r.clone();changed.span.signal_end=changed.span.full_frames;
    assert_eq!(plan(&changed,&c,&p,n),Err(Error::PttWindow));
    assert_eq!(original.end_presentation_ns-original.ptt_off_ns,1_660_000_000);
}
#[test]
fn changed_policy_or_signal_certificate_never_reuses_existing_plan(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    let mut changed=p.clone();changed.ptt_release_ns+=1;assert_eq!(original.revalidate_admission(&r,&c,&changed,n),Err(Error::Binding));
    let mut changed=r.clone();changed.span.signal_end-=1;assert_eq!(original.revalidate_admission(&changed,&c,&p,n),Err(Error::Binding));
}
#[test]
fn checked_arithmetic_rejects_overflow_in_frame_and_clock_mapping(){
    let (r,mut c,p,n)=fixture(false);c.output_anchor_frame=u64::MAX;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Overflow));
    let (r,mut c,p,n)=fixture(false);c.mono_anchor_ns=u64::MAX;c.measured_at_mono_ns=u64::MAX;
    assert!(plan(&r,&c,&p,n).is_err());
    assert_eq!(ceil_product(u64::MAX,u64::MAX,1),Err(Error::Overflow));
    assert_eq!(ceil_product(1,1,0),Err(Error::Invalid));
}
#[test]
fn empty_epochs_and_missing_budget_values_are_not_given_defaults(){
    let (mut r,c,p,n)=fixture(false);r.operation=0;assert_eq!(plan(&r,&c,&p,n),Err(Error::Invalid));
    for change in 0..5 {let (r,c,mut p,n)=fixture(false);match change {0=>p.epoch=0,
        1=>p.required_enqueue_lead_ns=0,2=>p.max_calibration_age_ns=0,
        3=>p.max_horizon_ns=0,_=>p.ptt_window_close_ns=0}
        assert!(plan(&r,&c,&p,n).is_err());}
}

#[test]
fn commissioned_channel_map_is_exact_even_on_initial_admission(){
    let (mut r,c,p,n)=fixture(false);r.channel_map=vec![0];
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Format));
    let (mut r,mut c,p,n)=fixture(false);r.channel_map=vec![0,1];c.channel_map=vec![1,0];
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Format));
    c.channel_map=r.channel_map.clone();plan(&r,&c,&p,n).unwrap();
    let original=plan(&r,&c,&p,n).unwrap();c.channel_map=vec![1,0];
    assert_eq!(original.revalidate_admission(&r,&c,&p,n),Err(Error::Binding));
}
#[test]
fn positive_uncertainty_expands_alignment_with_exact_boundary(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=10_000;c.output_anchor_mono_ns+=1;
    p.max_alignment_ns=10_001;plan(&r,&c,&p,n).unwrap();
    p.max_alignment_ns-=1;assert_eq!(plan(&r,&c,&p,n),Err(Error::Alignment));
    p.max_alignment_ns+=2;plan(&r,&c,&p,n).unwrap();
}
#[test]
fn positive_uncertainty_expands_both_ptt_window_edges(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=10_000;
    p.ptt_window_open_ns=100_000_000-10_000;
    p.ptt_window_close_ns=13_340_000_000+10_000;
    plan(&r,&c,&p,n).unwrap();
    p.ptt_window_open_ns+=1;assert_eq!(plan(&r,&c,&p,n),Err(Error::PttWindow));
    p.ptt_window_open_ns-=2;plan(&r,&c,&p,n).unwrap();
    p.ptt_window_close_ns-=1;assert_eq!(plan(&r,&c,&p,n),Err(Error::PttWindow));
    p.ptt_window_close_ns+=2;plan(&r,&c,&p,n).unwrap();
}
#[test]
fn positive_uncertainty_expands_ptt_duration_and_slot_end(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=10_000;
    p.max_ptt_duration_ns=13_240_000_000+20_000;
    plan(&r,&c,&p,n).unwrap();p.max_ptt_duration_ns-=1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::PttWindow));
    p.max_ptt_duration_ns=15*SECOND;p.ptt_window_close_ns=15*SECOND;
    p.ptt_release_ns=1_860_000_000-10_000;
    plan(&r,&c,&p,n).unwrap();p.ptt_release_ns+=1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::PttWindow));
}
#[test]
fn latest_possible_full_presentation_is_bounded_by_expiry_and_horizon(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=10_000;
    c.valid_until_mono_ns=46*SECOND+10_000;p.max_horizon_ns=17*SECOND+10_000;
    plan(&r,&c,&p,n).unwrap();c.valid_until_mono_ns-=1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Expired));c.valid_until_mono_ns+=2;
    plan(&r,&c,&p,n).unwrap();p.max_horizon_ns-=1;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Expired));p.max_horizon_ns+=2;
    plan(&r,&c,&p,n).unwrap();
}
#[test]
fn uncertainty_reduces_enqueue_deadline_with_exact_boundary(){
    let (r,mut c,p,n)=fixture(false);c.uncertainty_ns=10_000;
    let original=plan(&r,&c,&p,n).unwrap();let deadline=30_900_000_000-10_000;
    assert_eq!(original.commit_deadline_ns,deadline);
    for mono in [deadline-1,deadline] {
        original.revalidate_admission(&r,&c,&p,ClockSample {mono_ns:mono,utc_ns:mono-SECOND,..n}).unwrap();
    }
    assert_eq!(original.revalidate_admission(&r,&c,&p,
        ClockSample {mono_ns:deadline+1,utc_ns:deadline+1-SECOND,..n}),Err(Error::Late));
}
#[test]
fn admission_precedes_earliest_ptt_and_never_extends_past_slot(){
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=10_000;c.output_anchor_mono_ns+=50_000;
    p.required_enqueue_lead_ns=1;p.ptt_setup_ns=500_010_000;
    let original=plan(&r,&c,&p,n).unwrap();let deadline=31*SECOND;
    assert_eq!(original.commit_deadline_ns,deadline);
    assert!(original.ptt_on_ns-c.uncertainty_ns
        < original.first_presentation_ns-c.uncertainty_ns-p.required_enqueue_lead_ns);
    original.revalidate_admission(&r,&c,&p,
        ClockSample {mono_ns:deadline,utc_ns:deadline-SECOND,..n}).unwrap();
    // Later frame alignment never creates a catch-up admission window.
    assert_eq!(original.revalidate_admission(&r,&c,&p,
        ClockSample {mono_ns:deadline+1,utc_ns:deadline+1-SECOND,..n}),Err(Error::Late));
}
#[test]
fn admission_api_is_not_an_active_playback_heartbeat(){
    let (r,c,p,n)=fixture(false);let original=plan(&r,&c,&p,n).unwrap();
    assert_eq!(original.revalidate_admission(&r,&c,&p,
        ClockSample {mono_ns:original.signal_first_ns,utc_ns:original.signal_first_ns-SECOND,..n}),Err(Error::Late));
}
#[test]
fn drift_bound_covers_its_own_latest_endpoint_without_iteration(){
    let (_,mut c,_,_)=fixture(false);c.uncertainty_ns=100;c.drift_ppb=10;
    let end=46*SECOND;let u=presentation_uncertainty(&c,end).unwrap();assert_eq!(u,551);
    let required=c.uncertainty_ns+ceil_product(end+u-c.measured_at_mono_ns,c.drift_ppb,SECOND).unwrap();
    assert!(u>=required);
    let previous=u-1;
    assert!(previous<c.uncertainty_ns+ceil_product(end+previous-c.measured_at_mono_ns,c.drift_ppb,SECOND).unwrap());
    c.drift_ppb=SECOND;assert_eq!(presentation_uncertainty(&c,end),Err(Error::Invalid));
}
#[test]
fn uncertainty_arithmetic_never_wraps_at_u64_boundaries(){
    let (_,mut c,_,_)=fixture(false);c.uncertainty_ns=u64::MAX;c.drift_ppb=1;
    assert_eq!(presentation_uncertainty(&c,46*SECOND),Err(Error::Overflow));
    assert_eq!(add(u64::MAX,1),Err(Error::Overflow));assert_eq!(sub(0,1),Err(Error::Overflow));
    let (r,mut c,mut p,n)=fixture(false);c.uncertainty_ns=32*SECOND;
    p.max_uncertainty_ns=u64::MAX;p.max_alignment_ns=u64::MAX;
    p.max_horizon_ns=u64::MAX;
    assert_eq!(plan(&r,&c,&p,n),Err(Error::Overflow));
}
