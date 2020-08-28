/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
use futures::Sink;
use futures::unsync::mpsc::UnboundedSender;
use std::any::{Any, TypeId};
use std::cell::{RefCell, RefMut, Ref};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr;
use termion::event::Key;
use tokio_xmpp::Packet;
use xmpp_parsers::{Element, FullJid, BareJid, presence, iq};
use xmpp_parsers;

use crate::{contact, conversation};
use crate::message::Message;
use crate::command::{Command, CommandParser};
use crate::config::Config;
use crate::terminus::ViewTrait;

pub enum Event {
    Start,
    Connected(FullJid),
    #[allow(dead_code)]
    Disconnected(FullJid),
    Command(Command),
    CommandError(String),
    SendMessage(Message),
    Message(Message),
    Chat(BareJid),
    Join(FullJid),
    Iq(iq::Iq),
    Presence(presence::Presence),
    ReadPassword(Command),
    Win(String),
    Contact(contact::Contact),
    ContactUpdate(contact::Contact),
    Occupant{conversation: BareJid, occupant: conversation::Occupant},
    Signal(i32),
    LoadHistory(BareJid),
    Quit,
    Key(Key),
    Validate(Rc<RefCell<Option<(String, bool)>>>),
    GetInput(Rc<RefCell<Option<(String, usize, bool)>>>),
    AutoComplete(String, usize),
    ResetCompletion,
    Completed(String, usize),
    AddWindow(String, Option<Box<dyn ViewTrait<Event>>>),
    ChangeWindow(String),
}

pub trait Plugin: fmt::Display {
    fn new() -> Self where Self: Sized;
    fn init(&mut self, aparte: &Aparte) -> Result<(), ()>;
    fn on_event(&mut self, aparte: Rc<Aparte>, event: &Event);
}

pub trait AnyPlugin: Any + Plugin {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn as_plugin(&mut self) -> &mut dyn Plugin;
}

impl<T> AnyPlugin for T where T: Any + Plugin {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_plugin(&mut self) -> &mut dyn Plugin {
        self
    }
}

pub struct Password<T: FromStr>(pub T);

impl<T: FromStr> FromStr for Password<T> {
    type Err = T::Err;

    fn from_str(s: &str) -> Result<Self, T::Err> {
        match T::from_str(s) {
            Err(e) => Err(e),
            Ok(inner) => Ok(Password(inner)),
        }
    }
}

pub struct Connection {
    pub sink: UnboundedSender<Packet>,
    pub account: FullJid,
}

pub struct Aparte {
    pub commands: RefCell<HashMap<String, CommandParser>>,
    plugins: HashMap<TypeId, RefCell<Box<dyn AnyPlugin>>>,
    connections: RefCell<HashMap<String, Connection>>,
    current_connection: RefCell<Option<String>>,
    event_lock: RefCell<()>,
    event_queue: RefCell<Vec<Event>>,
    pub config: Config,

}

impl Aparte {
    pub fn new(config_path: PathBuf) -> Self {
        let mut config_file = match OpenOptions::new().read(true).write(true).create(true).open(config_path) {
            Err(err) => panic!("Cannot read config file {}", err),
            Ok(config_file) => config_file,
        };

        let mut config_str = String::new();
        if let Err(e) = config_file.read_to_string(&mut config_str) {
            panic!("Cannot read config file {}", e);
        }

        let config = match config_str.len() {
            0 => Config { accounts: HashMap::new() },
            _ => match toml::from_str(&config_str) {
                Err(err) => panic!("Cannot read config file {}", err),
                Ok(config) => config,
            },
        };

        Self {
            commands: RefCell::new(HashMap::new()),
            plugins: HashMap::new(),
            connections: RefCell::new(HashMap::new()),
            current_connection: RefCell::new(None),
            event_lock: RefCell::new(()),
            event_queue: RefCell::new(Vec::new()),
            config: config,
        }
    }

    pub fn add_command(&self, command: CommandParser) {
        self.commands.borrow_mut().insert(command.name.to_string(), command);
    }

    pub fn parse_command(self: Rc<Self>, command: Command) -> Result<(), String> {
        match Rc::clone(&self).commands.borrow().get(&command.args[0]) {
            Some(parser) => (parser.parser)(self, command),
            None => Err(format!("Unknown command {}", command.args[0])),
        }
    }

    pub fn add_plugin<T: 'static + fmt::Display + Plugin>(&mut self, plugin: T) {
        info!("Add plugin `{}`", plugin);
        self.plugins.insert(TypeId::of::<T>(), RefCell::new(Box::new(plugin)));
    }

    pub fn get_plugin<T: 'static>(&self) -> Option<Ref<T>> {
        let rc = match self.plugins.get(&TypeId::of::<T>()) {
            Some(rc) => rc,
            None => return None,
        };

        let any_plugin = rc.borrow();
        /* Calling unwrap here on purpose as we expect panic if plugin is not of the right type */
        Some(Ref::map(any_plugin, |p| p.as_any().downcast_ref::<T>().unwrap()))
    }

    pub fn get_plugin_mut<T: 'static>(&self) -> Option<RefMut<T>> {
        let rc = match self.plugins.get(&TypeId::of::<T>()) {
            Some(rc) => rc,
            None => return None,
        };

        let any_plugin = rc.borrow_mut();
        /* Calling unwrap here on purpose as we expect panic if plugin is not of the right type */
        Some(RefMut::map(any_plugin, |p| p.as_any_mut().downcast_mut::<T>().unwrap()))
    }

    pub fn add_connection(&self, account: FullJid, sink: UnboundedSender<Packet>) {
        let connection = Connection {
            account: account,
            sink: sink,
        };

        let account = connection.account.to_string();

        self.connections.borrow_mut().insert(account.clone(), connection);
        self.current_connection.replace(Some(account.clone()));
    }

    pub fn current_connection(&self) -> Option<FullJid> {
        let current_connection = self.current_connection.borrow();
        match &*current_connection {
            Some(current_connection) => {
                let connections = self.connections.borrow_mut();
                let connection = connections.get(&current_connection.clone()).unwrap();
                Some(connection.account.clone())
            },
            None => None,
        }
    }

    pub fn init(&self) -> Result<(), ()> {
        for (_, plugin) in self.plugins.iter() {
            if let Err(err) = plugin.borrow_mut().as_plugin().init(self) {
                return Err(err);
            }
        }

        Ok(())
    }

    pub fn start(self: Rc<Self>) {
        for (_, account) in self.config.accounts.clone() {
            if account.autoconnect {
                Rc::clone(&self).event(Event::Command(Command {
                    args: vec!["connect".to_string(), account.jid.clone()],
                    cursor: 0
                }));
            }
        }
    }

    pub fn send(&self, element: Element) {
        let mut raw = Vec::<u8>::new();
        element.write_to(&mut raw);
        debug!("SEND: {}", String::from_utf8(raw).unwrap());
        let packet = Packet::Stanza(element);
        // TODO use correct connection
        let mut connections = self.connections.borrow_mut();
        let current_connection = connections.iter_mut().next().unwrap().1;
        let mut sink = &current_connection.sink;
        if let Err(e) = sink.start_send(packet) {
            warn!("Cannot send packet: {}", e);
        }
    }

    pub fn event(self: Rc<Self>, event: Event) {
        self.event_queue.borrow_mut().push(event);
        if let Ok(_lock) = self.event_lock.try_borrow_mut() {
            while self.event_queue.borrow().len() > 0 {
                let event = self.event_queue.borrow_mut().remove(0);
                for (_, plugin) in self.plugins.iter() {
                    plugin.borrow_mut().as_plugin().on_event(Rc::clone(&self), &event);
                }

                match event {
                    Event::Start => {
                        Rc::clone(&self).start();
                    }
                    Event::Command(command) => {
                        match Rc::clone(&self).parse_command(command) {
                            Err(err) => Rc::clone(&self).log(err),
                            Ok(()) => {},
                        }
                    },
                    Event::SendMessage(message) => {
                        Rc::clone(&self).event(Event::Message(message.clone()));
                        if let Ok(xmpp_message) = Element::try_from(message) {
                            self.send(xmpp_message);
                        }
                    },
                    _ => {},
                }
            }
        }
    }

    pub fn log(self: Rc<Self>, message: String) {
        let message = Message::log(message);
        self.event(Event::Message(message));
    }
}
