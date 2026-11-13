use radio_link::sim::*;
use std::collections::HashSet;

pub const VIEW: usize = 30;
const FANOUT: usize = 8;
const CHANGE_BYTES: u32 = 512;

pub struct Push {
    pub view: Vec<Member>,
    seen: HashSet<u32>,
}
#[derive(Clone)]
pub enum Msg {
    Change(Change),
    Welcome(Vec<Member>),
}

impl Push {
    pub fn start(me: Member, boot: Boot, ctx: &mut Ctx<'_, Msg>) -> Self {
        let view = match boot {
            Boot::Founding { members } if members <= VIEW as u32 + 1 => {
                (0..members).filter(|m| *m != me).collect()
            }
            Boot::Founding { members } => {
                let mut view = Vec::new();
                while view.len() < VIEW {
                    let member = ctx.random(members);
                    if member != me && !view.contains(&member) {
                        view.push(member);
                    }
                }
                view
            }
            Boot::Joining { inviter } => vec![inviter],
        };
        Self {
            view,
            seen: HashSet::new(),
        }
    }
    fn edit(&mut self, change: Change) {
        match change.kind {
            Kind::Join => {
                if self.view.len() < VIEW && !self.view.contains(&change.subject) {
                    self.view.push(change.subject);
                }
            }
            Kind::Remove | Kind::Leave => self.view.retain(|m| *m != change.subject),
        }
    }
    fn forward(&self, change: Change, from: Member, ctx: &mut Ctx<'_, Msg>) {
        let mut choices: Vec<_> = self
            .view
            .iter()
            .copied()
            .filter(|m| *m != change.subject && *m != from && *m != ctx.me())
            .collect();
        for _ in 0..FANOUT.min(choices.len()) {
            let index = ctx.random(choices.len() as u32) as usize;
            ctx.send(choices.swap_remove(index), Msg::Change(change));
        }
    }
}
impl Node for Push {
    type Msg = Msg;
    fn size(msg: &Msg) -> u32 {
        match msg {
            Msg::Change(_) => CHANGE_BYTES,
            Msg::Welcome(view) => 16 + 32 * view.len() as u32,
        }
    }
    fn originate(&mut self, change: Change, ctx: &mut Ctx<'_, Msg>) {
        self.seen.insert(change.id);
        if change.kind == Kind::Join {
            ctx.send(
                change.subject,
                Msg::Welcome(
                    self.view
                        .iter()
                        .copied()
                        .filter(|m| *m != change.subject)
                        .collect(),
                ),
            );
            if self.view.len() == VIEW {
                let index = ctx.random(VIEW as u32) as usize;
                self.view[index] = change.subject;
            } else {
                self.edit(change);
            }
        } else {
            self.edit(change);
        }
        self.forward(change, ctx.me(), ctx);
    }
    fn receive(&mut self, from: Member, msg: Msg, ctx: &mut Ctx<'_, Msg>) {
        match msg {
            Msg::Change(change) if self.seen.insert(change.id) => {
                ctx.applied(change.id);
                self.edit(change);
                self.forward(change, from, ctx);
            }
            Msg::Welcome(view) => {
                self.view.clear();
                for member in std::iter::once(from).chain(view) {
                    if member != ctx.me() && !self.view.contains(&member) && self.view.len() < VIEW
                    {
                        self.view.push(member);
                    }
                }
            }
            _ => {}
        }
    }
    fn timer(&mut self, _: u64, _: &mut Ctx<'_, Msg>) {}
    fn undelivered(&mut self, _: Member, _: Msg, _: &mut Ctx<'_, Msg>) {}
}
