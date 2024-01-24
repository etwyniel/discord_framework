use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{
    parse_macro_input, spanned::Spanned, Attribute, Data, DeriveInput, Fields, FieldsNamed,
    FieldsUnnamed, GenericArgument, Lit, Meta, NestedMeta, PathArguments, Type,
};

struct Attr {
    key: String,
    value: String,
}

struct CommandOption {
    name: String,
    required: bool,
    autocomplete: bool,
    getter: proc_macro2::TokenStream,
    kind: proc_macro2::TokenStream,
    description: String,
}

fn get_attr_value(attrs: &[Attr], name: &str) -> syn::Result<Option<String>> {
    Ok(attrs
        .iter()
        .find(|a| a.key == name)
        .map(|a| a.value.clone()))
}

fn get_attr_list(attrs: &[Attribute]) -> Option<Vec<Attr>> {
    match attrs
        .iter()
        .find(|a| a.path.is_ident("cmd"))?
        .parse_meta()
        .unwrap()
    {
        Meta::List(list) => Some(
            list.nested
                .into_iter()
                .filter_map(|attr| match attr {
                    NestedMeta::Meta(Meta::NameValue(nv)) => {
                        let ident = nv.path.get_ident().unwrap();
                        let key = ident.to_string();
                        let value = match nv.lit {
                            Lit::Str(s) => s.value(),
                            _ => String::new(),
                        };
                        Some(Attr { key, value })
                    }
                    NestedMeta::Meta(Meta::Path(p)) => {
                        let ident = p.get_ident().unwrap();
                        let key = ident.to_string();
                        Some(Attr {
                            key,
                            value: String::new(),
                        })
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }
}

fn check_type_is_message(span: Span, ty: &Type) -> syn::Result<()> {
    if let Type::Path(path) = ty {
        let segs = &path.path.segments;
        let parts = segs
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
        if ["Message", "serenity::model::channel::Message"].contains(&parts.as_str()) {
            return Ok(());
        }
    }
    Err(syn::Error::new(
        span,
        "Command on messages must have one field of type message",
    ))
}

fn analyze_message_command_fields(
    ident: &syn::Ident,
    fields: Fields,
) -> syn::Result<proc_macro2::TokenStream> {
    let setter = match fields {
        Fields::Named(FieldsNamed { named, .. }) if named.len() == 1 => {
            let f = named.first().unwrap();
            check_type_is_message(f.span(), &f.ty)?;
            let fident = f.ident.as_ref().unwrap();
            quote!(#ident {
                #fident: msg.clone(),
            })
        }
        Fields::Unnamed(FieldsUnnamed { unnamed, .. }) if unnamed.len() == 1 => {
            let f = unnamed.first().unwrap();
            check_type_is_message(f.span(), &f.ty)?;
            quote!(#ident(msg.clone()))
        }
        _ => {
            return Err(syn::Error::new(
                ident.span(),
                "Command on messages must have one field of type message",
            ))
        }
    };
    Ok(
        quote!(if let Some(msg) = opts.resolved.messages.values().next() {
            #setter
        } else {
            panic!("No message received for message command")
        }),
    )
}

fn analyze_field(
    ident: &syn::Ident,
    mut ty: &Type,
    attrs: &[Attribute],
) -> syn::Result<CommandOption> {
    let attrs = get_attr_list(attrs).unwrap_or_default();
    let name = get_attr_value(&attrs, "name")?.unwrap_or_else(|| ident.to_string());
    let desc = get_attr_value(&attrs, "desc")?.unwrap_or_else(|| ident.to_string());
    let find_opt = quote!(opts.options.iter().find(|o| o.name == #name).map(|o| &o.value));
    let opt_value = quote!(serenity::model::application::CommandDataOptionValue);
    let mut required = true;
    let autocomplete = get_attr_value(&attrs, "autocomplete")?.is_some();
    if let Type::Path(path) = ty {
        let segs = &path.path.segments;
        if segs.len() == 1 && segs[0].ident == "Option" {
            required = false;
            if let PathArguments::AngleBracketed(args) = &segs[0].arguments {
                ty = match &args.args[0] {
                    GenericArgument::Type(ty) => ty,
                    _ => return Err(syn::Error::new(ident.span(), "Invalid option")),
                };
            }
        }
    }
    match ty {
        Type::Path(path) => {
            let segs = &path.path.segments;
            let parts = segs
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            let parts_str = parts.as_str();
            let (matcher, kind) = match parts_str {
                "String" | "std::str::String" => (
                    quote!(#opt_value::String(v)),
                    quote!(serenity::model::application::CommandOptionType::String),
                ),
                "i64" | "u64" | "usize" => (
                    quote!(#opt_value::Integer(v)),
                    quote!(serenity::model::application::CommandOptionType::Integer),
                ),
                "f64" => (
                    quote!(#opt_value::Number(v)),
                    quote!(serenity::model::application::CommandOptionType::Number),
                ),
                "bool" => (
                    quote!(#opt_value::Boolean(v)),
                    quote!(serenity::model::application::CommandOptionType::Boolean),
                ),
                "RoleId" | "serenity::model::guild::RoleId" => (
                    quote!(#opt_value::Role(v)),
                    quote!(serenity::model::application::CommandOptionType::Role),
                ),
                "User" | "serenity::model::user::User" => (
                    quote!(#opt_value::User(v)),
                    quote!(serenity::model::application::CommandOptionType::User),
                ),
                "UserId" | "serenity::model::user::UserId" => (
                    quote!(#opt_value::User(v)),
                    quote!(serenity::model::application::CommandOptionType::User),
                ),
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("Unsupported type {other}"),
                    ))
                }
            };
            let cast = if let "i64" | "u64" | "usize" | "isize" | "u32" | "i32" = parts_str {
                let id = Ident::new(parts_str, Span::call_site());
                quote!( as #id )
            } else {
                quote!()
            };
            let getter = if required {
                quote!(if let Some(#matcher) = #find_opt {
                    v.clone() #cast
                } else {
                    panic!("Value is required")
                })
            } else {
                quote!(if let Some(#matcher) = #find_opt {
                    Some(v.clone() #cast)
                } else {
                    None
                })
            };
            Ok(CommandOption {
                name: ident.to_string(),
                required,
                autocomplete,
                getter,
                kind,
                description: desc,
            })
        }
        _ => Err(syn::Error::new(ident.span(), "Unsupported type")),
    }
}

impl CommandOption {
    fn create(&self) -> proc_macro2::TokenStream {
        let name = &self.name;
        let desc = &self.description;
        let kind = &self.kind;
        let required = self.required;
        let autocomplete = self.autocomplete;
        quote!(builder = builder.add_option({
            let mut opt = serenity::builder::CreateCommandOption::new(#kind, #name, #desc)
                .required(#required)
                .set_autocomplete(#autocomplete);
            opt = (&extras)(#name, opt);
            opt
        });)
    }
}

fn derive(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let DeriveInput {
        ident,
        generics,
        data,
        attrs,
        ..
    } = input;
    if !generics.params.is_empty() {
        return Err(syn::Error::new(
            ident.span(),
            "Generic structs are not supported",
        ));
    }
    let attrs = get_attr_list(&attrs).unwrap_or_default();
    let s = match data {
        Data::Struct(s) => s,
        _ => {
            return Err(syn::Error::new(
                ident.span(),
                "Derive target must be a struct",
            ))
        }
    };
    let attr_name = get_attr_value(&attrs, "name")?;
    let name = attr_name.unwrap_or_else(|| ident.to_string());
    let desc = get_attr_value(&attrs, "desc")?.unwrap_or_else(|| ident.to_string());
    let message = get_attr_value(&attrs, "message")?.is_some();
    let (constructor, builders, set_desc, set_type) = if message {
        let constructor = analyze_message_command_fields(&ident, s.fields)?;
        let builder =
            quote!(builder = builder.kind(serenity::model::application::CommandType::Message););
        let set_type = quote!(
            const TYPE: serenity::model::application::CommandType =
                serenity::model::application::CommandType::Message;
        );
        (constructor, vec![builder], quote!(), set_type)
    } else {
        let fields = match s.fields {
            Fields::Named(f) => f,
            Fields::Unit => FieldsNamed {
                brace_token: syn::token::Brace {
                    span: Span::call_site(),
                },
                named: Default::default(),
            },
            _ => {
                return Err(syn::Error::new(
                    ident.span(),
                    "Derive target must use named fields",
                ))
            }
        };
        let field_names = fields.named.iter().flat_map(|f| f.ident.as_ref());
        let opts: Vec<_> = fields
            .named
            .iter()
            .map(|f| analyze_field(f.ident.as_ref().unwrap(), &f.ty, &f.attrs))
            .collect::<syn::Result<_>>()?;
        let builders = opts.iter().map(CommandOption::create).collect();
        let getters = opts.iter().map(|o| &o.getter);
        let constructor = quote!(#ident {
            #(#field_names: #getters),*
        });
        let set_desc = quote!(builder = builder.description(#desc););
        (constructor, builders, set_desc, quote!())
    };
    let runner_ident = Ident::new(&format!("__{}_runner", &ident), Span::call_site());
    let app_command = quote!(serenity::model::application);
    let data_ident = quote!(<#ident as serenity_command::BotCommand>::Data);
    Ok(quote!(
            impl<'a> From<&'a #app_command::CommandData> for #ident {
                fn from(opts: &'a #app_command::CommandData) -> Self {
                    #constructor
                }
            }

            #[allow(non_camel_case_types)]
            struct #runner_ident;

            #[async_trait]
            impl serenity_command::CommandRunner<#data_ident> for #runner_ident {
                async fn run(
                    &self,
                    data: &#data_ident,
                    ctx: &serenity::prelude::Context,
                    interaction: &#app_command::CommandInteraction,
                    ) -> anyhow::Result<serenity_command::CommandResponse> {
                    #ident::from(&interaction.data).run(data, ctx, interaction).await
                }

                fn name(&self) -> serenity_command::CommandKey<'static> {
                    (<#ident as serenity_command::CommandBuilder>::NAME, <#ident as serenity_command::CommandBuilder>::TYPE)
                }

                fn register<'a>(&self) -> serenity::builder::CreateCommand {
                    use serenity_command::CommandBuilder;
                    let mut builder = serenity::builder::CreateCommand::new(<#ident as serenity_command::CommandBuilder>::NAME);
                    builder = #ident::create_extras(builder, <#ident as serenity_command::BotCommand>::setup_options);
                    if !#ident::PERMISSIONS.is_empty() {
                        builder = builder.default_member_permissions(#ident::PERMISSIONS);
                    }
                    builder
                }

                fn guild(&self) -> Option<serenity::model::prelude::GuildId> {
                    #ident::GUILD
                }
            }

        impl<'a> serenity_command::CommandBuilder<'a> for #ident {
        fn create_extras<E: Fn(&'static str, serenity::builder::CreateCommandOption) -> serenity::builder::CreateCommandOption>(
            mut builder: serenity::builder::CreateCommand,
            extras: E
        ) -> serenity::builder::CreateCommand {
            #set_desc
            builder = builder.name(#name);
            #(#builders)*
            builder
        }

        fn create(builder: serenity::builder::CreateCommand)
            -> serenity::builder::CreateCommand
        {
            let extras = |_: &'static str, opt: serenity::builder::CreateCommandOption| {opt};
            Self::create_extras(builder, extras)
        }

        const NAME: &'static str = #name;
        #set_type

        fn runner() -> Box<dyn serenity_command::CommandRunner<Self::Data> + Send + Sync> {
            Box::new(#runner_ident)
        }
    }))
}

#[proc_macro_derive(Command, attributes(cmd))]
pub fn derive_serenity_command(input: TokenStream) -> TokenStream {
    derive(parse_macro_input!(input))
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}
