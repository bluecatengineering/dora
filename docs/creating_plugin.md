# Creating a Plugin

Plugins for dora are like middleware for HTTP servers. They modify the incoming message in some way and prepare the response. They additionally have the ability to short-circuit request processing by returning early, telling the server to respond or drop the message.

Proc macros are included to make writing plugins easy. A basic plugin looks like this:

```rust
use dora_core::{
    dhcproto::v4::Message,
    prelude::*,
};
use config::DhcpConfig;

#[derive(Debug, Register)]
#[register(msg(Message))]
#[register(plugin())]
pub struct MyPlugin {
    cfg: Arc<DhcpConfig>,
}

#[async_trait]
impl Plugin<Message> for MyPlugin {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        Ok(Action::Continue)
    }
}
```

After deriving the `Register` trait, you can use `#[register(msg(Message))]` to say that you want to 'register' this plugin to handle `dhcproto::v4::Message` type messages, DHCPv4 messages essentially. You can add an additional `#[register(msg(v6::Message))]` attribute if a plugin will be run on v6 messages also. Each `register(msg())` attribute requires a corresponding `impl Plugin<>` implementation.

The `#[register(plugin())]` attribute tells dora what other plugins you want to run _before_ this plugin runs. This way you can make sure some other plugin always runs before `MyPlugin`. You can put multiple entries here to create dependencies. At startup, dora will do a topological sort to create a dependency path through the plugins which it will use at runtime.

The `handle` method is where all the fun stuff happens. You can modify the `MsgContext<v4::Message>` and can return `Action::Continue`, `Action::Respond`, or `Action::NoResponse`.
