# register_derive

derive macros for easily defining plugins for dora.

ex.

```rust
use dora_core::{
    dhcproto::v4::Message,
    prelude::*,
};
use register_derive::Register;
use message_type::MsgType;

#[derive(Register)]
#[register(msg(Message))]
#[register(plugin(MsgType))]
pub struct PluginName;
```

Defines a new plugin called `PluginName` and a `Register` implementation. It is defined with a `v4::Message` type, i.e. for dhcpv4 only, and has a dependency on the `MsgType` plugin, meaning `MsgType` will always be called before `PluginName` in the plugin handler sequence. All of this will generate code that looks roughly like:

```rust
#[automatically_derived]
impl dora_core::Register<Message> for StaticAddr {
    fn register(self, srv: &mut dora_core::Server<Message>) {
         // some logging stuff ommitted
        let this = std::sync::Arc::new(self);
        srv.plugin_order::<Self, _>(this, &[std::any::TypeId::of::<MsgType>()]);
    }
}
```

**TODO**: automatic derives for generic message types not currently supported, i.e. if you want to derive `Register` for a plugin that is generic over v4/v6 (`T: Encodable + Decodable`) you will need to write a `Register` implementation by hand. We could improve the `register_derive_impl` so that if you don't include `#[register(Message)]` it will assume you want to define it generically, but this is not yet done.
