use quote::quote;

use crate::model;

pub(crate) fn emit_excel_function(plan: &model::UdfPlan) -> proc_macro2::TokenStream {
    let function = &plan.function;
    let krate = &plan.crate_path;
    let gating = &plan.gating;
    let return_type = &plan.return_type;
    let function_ident = &function.sig.ident;
    let udf_id = plan.metadata.id.as_str();
    let excel_name = &plan.metadata.excel_name;
    let category = &plan.metadata.category;
    let description = &plan.metadata.description;
    let help_topic = &plan.metadata.help_topic;
    let export_ident = &plan.symbols.export_ident;
    let descriptor_ident = &plan.symbols.descriptor_ident;
    let export_manifest_entry_ident = &plan.symbols.manifest_ident;
    let export_name_bytes = syn::LitByteStr::new(
        &plan.symbols.export_name_bytes,
        proc_macro2::Span::call_site(),
    );
    let execution = &plan.execution;
    let is_async = execution.is_async();
    let context_type = execution.context_type();
    let arguments = &plan.arguments;
    let has_handle_lease = arguments
        .iter()
        .any(|argument| argument.conversion.is_handle_lease());
    let raw_names = arguments
        .iter()
        .map(|argument| argument.raw_ident.clone())
        .collect::<Vec<_>>();
    let converted_names = arguments
        .iter()
        .map(|argument| argument.local_ident.clone())
        .collect::<Vec<_>>();
    let argument_name_literals = arguments
        .iter()
        .map(|argument| argument.excel_name.clone())
        .collect::<Vec<_>>();
    let argument_descriptions = arguments
        .iter()
        .map(|argument| argument.description.clone())
        .collect::<Vec<_>>();
    let argument_count = arguments.len();
    let generated_context_expression = match execution {
        model::ExecutionSpec::ThreadSafe {
            context_ty: Some(_),
        } => Some(quote!(#krate::__private::v1::thread_safe_context(__state))),
        model::ExecutionSpec::MainThread {
            context_ty: Some(_),
        } => Some(quote!(#krate::__private::v1::main_thread_context(
            __frame,
            __state,
        ))),
        model::ExecutionSpec::MacroSheet {
            context_ty: Some(_),
        } => Some(quote!(#krate::__private::v1::macro_sheet_context(
            __frame,
            __state,
        ))),
        model::ExecutionSpec::Async {
            context_ty: Some(_),
        } => Some(quote!(#krate::__private::v1::async_context(&__lease, &__cancellation))),
        model::ExecutionSpec::MainThread { context_ty: None }
        | model::ExecutionSpec::ThreadSafe { context_ty: None }
        | model::ExecutionSpec::MacroSheet { context_ty: None }
        | model::ExecutionSpec::Async { context_ty: None } => None,
    };
    let hidden = plan.hidden;
    let invocation = if generated_context_expression.is_some() {
        quote!(#function_ident(__context, #(#converted_names),*))
    } else {
        quote!(#function_ident(#(#converted_names),*))
    };
    let async_result_expression = quote! {
        let __result = #invocation.await;
        ::core::result::Result::Ok(__result)
    };
    let context_setup = generated_context_expression.map(|expression| {
        let ty = context_type
            .as_ref()
            .expect("context expression always has a declared type");
        quote! {
            let __generated_context = #expression;
            let __context: #ty = __generated_context;
        }
    });
    let volatile = plan.volatile;
    let execution_kind = match execution {
        model::ExecutionSpec::MainThread { .. } => {
            quote!(#krate::__private::v1::ExecutionKind::MainThread)
        }
        model::ExecutionSpec::ThreadSafe { .. } => {
            quote!(#krate::__private::v1::ExecutionKind::ThreadSafe)
        }
        model::ExecutionSpec::MacroSheet { .. } => {
            quote!(#krate::__private::v1::ExecutionKind::MacroSheet)
        }
        model::ExecutionSpec::Async { .. } => {
            quote!(#krate::__private::v1::ExecutionKind::Async)
        }
    };
    let visibility = if hidden {
        quote!(#krate::__private::v1::FunctionVisibility::Hidden)
    } else {
        quote!(#krate::__private::v1::FunctionVisibility::Public)
    };
    let mode_assertion = match execution {
        model::ExecutionSpec::Async { .. } => {
            quote!(#krate::__private::v1::assert_async_return::<#return_type>();)
        }
        model::ExecutionSpec::MacroSheet { .. } => {
            quote!(#krate::__private::v1::assert_macro_sheet_return::<#return_type>();)
        }
        model::ExecutionSpec::ThreadSafe { .. } => {
            quote!(#krate::__private::v1::assert_thread_safe_return::<#return_type>();)
        }
        model::ExecutionSpec::MainThread { .. } => {
            quote!(#krate::__private::v1::assert_main_thread_return::<#return_type>();)
        }
    };
    let volatile_assertion =
        volatile.then(|| quote!(#krate::__private::v1::assert_volatile_return::<#return_type>();));
    let return_assertion = quote! {
        #mode_assertion
        #volatile_assertion
    };
    let conversions = arguments
        .iter()
        .map(|argument| {
            let index = argument.index;
            let ty = &argument.ty;
            let converted = &argument.local_ident;
            let raw = &argument.raw_ident;
            let argument_name = &argument.excel_name;
            let conversion = match &argument.conversion {
                model::ArgumentConversion::Reference => quote! {
                    // SAFETY: Excel supplies the live reference pointer for this ABI call.
                    unsafe {
                        #krate::__private::v1::convert_reference(__frame, #argument_name, #raw)
                    }
                },
                model::ArgumentConversion::Value(_) => {
                    let async_assertion = is_async.then(|| {
                        quote!(#krate::__private::v1::assert_async_parameter::<#return_type, #ty>();)
                    });
                    quote! {
                        {
                            #async_assertion
                            #krate::__private::v1::assert_excel_parameter::<#return_type, #ty>(__frame);
                            // SAFETY: Excel supplies the live XLOPER12 pointer for this ABI call.
                            unsafe {
                                #krate::__private::v1::convert_argument::<#return_type, #ty>(
                                    __frame,
                                    #index,
                                    #argument_name,
                                    #raw,
                                )
                            }
                        }
                    }
                }
                model::ArgumentConversion::HandleLease { object } => quote! {
                    // The pending pin is acquired while the Excel call is
                    // admitted and branded only inside the async task builder.
                    unsafe {
                        __frame.convert_handle_pending::<_, #object>(
                            #index,
                            #argument_name,
                            #raw,
                            &__lease,
                        )
                    }
                },
            };

            if argument.conversion.requires_presence_check() {
                let model::ArgumentConversion::Value(value) = &argument.conversion
                else {
                    unreachable!("reference conversion does not inspect cell presence");
                };
                let blank = &value.blank;
                let missing = &value.missing;
                let blank_arm = match blank {
                    model::PresenceAction::Convert => quote!(),
                    model::PresenceAction::Error => quote!(
                        #krate::__private::v1::CellPresence::Blank => return ::core::result::Result::Err(
                            #krate::error::XllError::input(
                                #argument_name,
                                #krate::error::InputError::Malformed("blank cell is not allowed"),
                            )
                        ),
                    ),
                    model::PresenceAction::Default(default) => quote!(
                        #krate::__private::v1::CellPresence::Blank => {
                            #krate::__private::v1::CallFrame::default_argument(
                                __frame,
                                #index,
                                #argument_name,
                                #default,
                            )?
                        },
                    ),
                };
                let missing_arm = match missing {
                    model::PresenceAction::Convert => quote!(),
                    model::PresenceAction::Error => quote!(
                        #krate::__private::v1::CellPresence::Missing => return ::core::result::Result::Err(
                            #krate::error::XllError::input(
                                #argument_name,
                                #krate::error::InputError::Malformed("missing argument is not allowed"),
                            )
                        ),
                    ),
                    model::PresenceAction::Default(default) => quote!(
                        #krate::__private::v1::CellPresence::Missing => {
                            #krate::__private::v1::CallFrame::default_argument(
                                __frame,
                                #index,
                                #argument_name,
                                #default,
                            )?
                        },
                    ),
                };
                quote! {
                    // SAFETY: the raw argument belongs to the current Excel
                    // call and is validated by the conversion boundary.
                    let #converted: #ty = match unsafe {
                        #krate::__private::v1::argument_presence(__frame, #argument_name, #raw)
                    }? {
                        #blank_arm
                        #missing_arm
                        _ => #conversion?,
                    };
                }
            } else {
                if argument.conversion.is_handle_lease() {
                    quote! {
                        let #converted = #conversion?;
                    }
                } else {
                    quote! {
                        let #converted: #ty = #conversion?;
                    }
                }
            }
        })
        .collect::<Vec<_>>();

    let handle_bindings = arguments
        .iter()
        .filter_map(|argument| {
            if !argument.conversion.is_handle_lease() {
                return None;
            }
            let converted = &argument.local_ident;
            Some(quote! {
                let #converted = #krate::__private::v1::bind_handle_lease(
                    #converted,
                    __scope,
                );
            })
        })
        .collect::<Vec<_>>();

    let builder_ident = syn::Ident::new("__XlfnHandleTaskBuilder", proc_macro2::Span::mixed_site());
    let mut builder_struct_fields = Vec::with_capacity(arguments.len() + 2);
    for argument in arguments {
        let name = &argument.local_ident;
        let ty = &argument.ty;
        let field_type =
            if let model::ArgumentConversion::HandleLease { object } = &argument.conversion {
                quote!(#krate::__private::v1::PendingHandleLease<#object>)
            } else {
                quote!(#ty)
            };
        builder_struct_fields.push(quote!(#name: #field_type,));
    }
    let builder_addin_ident = syn::Ident::new("__XlfnHandleAddin", proc_macro2::Span::mixed_site());
    let builder_lease_field = context_type
        .is_some()
        .then(|| quote!(__lease: #krate::__private::v1::ExecutionLease<#builder_addin_ident>,));
    if let Some(field) = builder_lease_field {
        builder_struct_fields.push(field);
    }
    if context_type.is_some() {
        builder_struct_fields
            .push(quote!(__cancellation: #krate::__private::v1::CancellationToken,));
    }
    let builder_argument_moves = arguments
        .iter()
        .map(|argument| {
            let name = &argument.local_ident;
            quote!(let #name = self.#name;)
        })
        .collect::<Vec<_>>();
    let builder_lease_move = if context_type.is_some() {
        quote!(let __lease = self.__lease;)
    } else {
        quote!()
    };
    let builder_cancellation_move = context_type
        .map(|_| quote!(let __cancellation = self.__cancellation;))
        .unwrap_or_else(|| quote!());
    let mut builder_initializers = arguments
        .iter()
        .map(|argument| {
            let name = &argument.local_ident;
            quote!(#name: #name,)
        })
        .collect::<Vec<_>>();
    if context_type.is_some() {
        builder_initializers.push(quote!(__lease: __lease,));
    }
    if context_type.is_some() {
        builder_initializers.push(quote!(__cancellation: __cancellation,));
    }
    let context_type_for_bound = context_type.map(|context| {
        let mut context = context.clone();
        if let syn::Type::Path(path) = &mut context {
            for segment in &mut path.path.segments {
                if let syn::PathArguments::AngleBracketed(arguments) = &mut segment.arguments {
                    for argument in &mut arguments.args {
                        if let syn::GenericArgument::Lifetime(lifetime) = argument {
                            *lifetime = syn::parse_quote!('__xlfn_context);
                        }
                    }
                }
            }
        }
        context
    });
    let builder_context_setup = context_type.map(|context| {
        quote! {
            let __generated_context = #krate::__private::v1::async_context_for(
                &__lease,
                &__cancellation,
            );
            let __context: #context = __generated_context;
        }
    });
    let builder_context_setup = builder_context_setup.unwrap_or_else(|| quote!());
    let (builder_type, builder_struct, builder_impl_generics, builder_where) =
        if context_type.is_some() {
            let context_type_for_bound = context_type_for_bound
                .as_ref()
                .expect("async context types are path types");
            (
                quote!(#builder_ident<#builder_addin_ident>),
                quote! {
                    struct #builder_ident<#builder_addin_ident: #krate::Addin> {
                        #(#builder_struct_fields)*
                    }
                },
                quote!(impl<#builder_addin_ident: #krate::Addin>),
                quote! {
                    where
                        for<'__xlfn_context> #context_type_for_bound:
                            #krate::__private::v1::AsyncContextFactory<
                                '__xlfn_context,
                                #builder_addin_ident,
                            >
                },
            )
        } else {
            (
                quote!(#builder_ident),
                quote! {
                    struct #builder_ident {
                        #(#builder_struct_fields)*
                    }
                },
                quote!(impl),
                quote!(),
            )
        };
    let builder_definition = if has_handle_lease {
        quote! {
            #builder_struct

            #builder_impl_generics
                #krate::__private::v1::HandleScopedBuilder<#return_type>
                for #builder_type
            #builder_where
            {
                fn build<'generation>(
                    self,
                    __scope: #krate::__private::v1::AsyncTaskScope<'generation>,
                ) -> ::std::pin::Pin<
                    ::std::boxed::Box<
                        dyn ::std::future::Future<
                                Output = #krate::error::XllResult<#return_type>,
                            > + Send
                            + 'generation,
                    >,
                > {
                    #(#builder_argument_moves)*
                    #builder_lease_move
                    #builder_cancellation_move
                    #(#handle_bindings)*
                    #krate::__private::v1::box_scoped_future(__scope, async move {
                        #builder_context_setup
                        #return_assertion
                        #async_result_expression
                    })
                }
            }
        }
    } else {
        quote!()
    };

    let boundary = if is_async {
        if has_handle_lease {
            quote! {
                #krate::__private::v1::__xlfn_async_only! {
                    // SAFETY: `__async_handle` is provided by Excel via the extern "system"
                    // entry point generated by this macro and points to a valid async handle.
                    unsafe {
                        #krate::__private::v1::async_udf_handle::<_, #return_type, _, _>(
                            &crate::__XLFN_RUNTIME,
                            #udf_id,
                            #excel_name,
                            #argument_count,
                            __async_handle,
                            |__call, __lease, __cancellation, __frame| {
                                #builder_definition
                                #(#conversions)*
                                ::core::result::Result::Ok(#builder_ident {
                                    #(#builder_initializers)*
                                })
                            },
                        )
                    }
                }
            }
        } else {
            quote! {
                #krate::__private::v1::__xlfn_async_only! {
                    // SAFETY: `__async_handle` is provided by Excel via the extern "system"
                    // entry point generated by this macro and points to a valid async handle.
                    unsafe {
                        #krate::__private::v1::async_udf::<_, #return_type, _, _>(
                            &crate::__XLFN_RUNTIME,
                            #udf_id,
                            #excel_name,
                            #argument_count,
                            __async_handle,
                            |__call, __lease, __cancellation, __frame| {
                                #(#conversions)*
                                ::core::result::Result::Ok(async move {
                                    #context_setup
                                    #return_assertion
                                    #async_result_expression
                                })
                            },
                        )
                    }
                }
            }
        }
    } else {
        quote! {
            #krate::__private::v1::sync_udf::<_, #return_type, _>(
                &crate::__XLFN_RUNTIME,
                #udf_id,
                #excel_name,
                #argument_count,
                |__state, __frame| {
                    #return_assertion
                    #context_setup
                    #(#conversions)*
                    let mut __return_context =
                        __frame.return_context(#udf_id)?;
                    #krate::__private::v1::ExcelReturn::invoke(
                        &mut __return_context,
                        || ::core::result::Result::Ok(#invocation),
                    )
                },
            )
        }
    };
    let wrapper = if is_async {
        quote! {
            #(#gating)*
            #[doc = concat!("Excel async ABI wrapper for `", #excel_name, "`.")]
            #[doc = ""]
            #[doc = "# Safety"]
            #[doc = "Every argument pointer and the async handle must be a live XLOPER12 supplied by Excel for this call."]
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn #export_ident(
                #(#raw_names: *mut #krate::__private::v1::XLOPER12,)*
                __async_handle: *mut #krate::__private::v1::XLOPER12,
            ) {
                #boundary
            }
        }
    } else {
        quote! {
            #(#gating)*
            #[doc = concat!("Excel ABI wrapper for `", #excel_name, "`.")]
            #[doc = ""]
            #[doc = "# Safety"]
            #[doc = "Every argument pointer must be a live XLOPER12 supplied by Excel for this call."]
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn #export_ident(
                #(#raw_names: *mut #krate::__private::v1::XLOPER12),*
            ) -> *mut #krate::__private::v1::XLOPER12 {
                #boundary
            }
        }
    };

    let argument_abi_tokens = arguments.iter().map(|argument| {
        if argument.conversion.is_reference() {
            quote!(#krate::__private::v1::ArgumentAbi::RawReference)
        } else {
            quote!(#krate::__private::v1::ArgumentAbi::CoercedValue)
        }
    });

    quote! {
        #function

        #(#gating)*
        #[doc(hidden)]
        #[allow(non_upper_case_globals, reason = "Generated registration descriptor identifier")]
        static #descriptor_ident: #krate::__private::v1::FunctionRegistration =
            #krate::__private::v1::FunctionRegistration::new(
                stringify!(#export_ident),
                #excel_name,
                #category,
                #description,
                #help_topic,
                &[
                    #(
                        #krate::__private::v1::ArgumentDescriptor {
                            name: #argument_name_literals,
                            description: #argument_descriptions,
                        },
                    )*
                ],
                &[
                    #(#argument_abi_tokens),*
                ],
                #execution_kind,
                #volatile,
                #visibility,
            );

        #(#gating)*
        #krate::__private::v1::submit_registration! {
            #descriptor_ident
        }

        #(#gating)*
        #[doc(hidden)]
        #[allow(non_upper_case_globals, reason = "Generated export symbol identifier")]
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static #export_manifest_entry_ident: [u8; #export_name_bytes.len()] =
            *#export_name_bytes;

        #wrapper
    }
}
